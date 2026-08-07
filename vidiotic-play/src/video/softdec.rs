//! Block-compressed → RGBA8 on the CPU: the fallback for a device without
//! `texture-compression-bc`.
//!
//! HAP is block-compressed by construction, so a GPU that cannot sample BC
//! textures cannot show a HAP clip at all — `render::upload_frame` would build
//! a `Bc1RgbaUnorm` texture the device refuses, and before this module existed
//! the browser's answer to that was a black canvas with a line in the console.
//! Desktop GPUs all have BC; Apple silicon, Android, and most integrated mobile
//! parts do not, and they are exactly the machines a link gets opened on.
//!
//! # What this has to agree with
//!
//! Not the GPU's BC decoder — BC1/BC3 endpoint interpolation is explicitly
//! implementation-defined, so bit-equality is neither achievable nor required.
//! What it *does* have to agree with is
//! `vidiotic-core/shaders/preamble.frag`'s `video()`: the shader's job on the
//! BC path is to unswizzle scaled-YCoCg and to expand an alpha-only texture,
//! and on this path the shader never gets the chance, because
//! [`PixelData::Rgba`] reports `video_mode` 0. So every branch of `video()` is
//! reproduced here instead, against the same constants. A mismatch is not a
//! crash, it is a clip that looks wrong on one class of machine and right on
//! every machine the author owns — which is why the tests below check the
//! transform and not just the block layout.
//!
//! # Cost
//!
//! One pass over every pixel, scalar, plus one allocation for the output. It
//! runs only when the timeline moves to a new sample (`web::Engine::pull_frame`
//! already gates on that), so a 30 fps clip on a 60 Hz display pays it on half
//! the frames. At the §3a chop tier that is a fraction of a frame's budget; at
//! 1080p it is not free, and the readout says which path is running so a slow
//! machine is legible rather than mysterious.

use vidiotic_bake::hap::HapTextureFormat;

use super::frame::{DecodedFrame, PixelData};

/// Why a frame could not be decompressed in software.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftDecErr {
    /// BC7 has eight partitioned modes with per-mode endpoint layouts — a
    /// decoder several times the size of everything else here. Nothing in this
    /// repo emits it (`transcode.rs` bakes Hap1/BC1 only), and it is named
    /// rather than silently skipped so a clip from another tool that *does*
    /// use Hap R reports the reason instead of showing black.
    Unsupported(HapTextureFormat),
    /// The payload is shorter than the frame's block grid implies.
    Truncated { need: usize, got: usize },
    /// The alpha plane of a `HapM` frame is missing or short.
    AlphaTruncated { need: usize, got: usize },
}

impl std::fmt::Display for SoftDecErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(fmt) => write!(
                f,
                "{fmt:?} has no software decoder; this GPU needs texture-compression-bc for it"
            ),
            Self::Truncated { need, got } => {
                write!(f, "frame payload is {got} bytes, needs {need}")
            }
            Self::AlphaTruncated { need, got } => {
                write!(f, "alpha plane is {got} bytes, needs {need}")
            }
        }
    }
}

impl std::error::Error for SoftDecErr {}

/// Blocks across and down for a `w`x`h` image, rounding up.
const fn block_grid(w: u32, h: u32) -> (usize, usize) {
    ((w as usize).div_ceil(4), (h as usize).div_ceil(4))
}

/// Turn a block-compressed frame into one `render::upload_frame` can upload to
/// a device with no BC support.
///
/// An [`PixelData::Rgba`] frame is returned unchanged (cloned), so callers can
/// apply this unconditionally on the fallback path without inspecting the
/// payload first.
///
/// # Errors
/// [`SoftDecErr`] if the format has no software decoder or a payload is short.
pub fn to_rgba(frame: &DecodedFrame) -> Result<DecodedFrame, SoftDecErr> {
    let (w, h) = (frame.w, frame.h);
    let (format, data, alpha, video_mode) = match &frame.pixels {
        PixelData::Bc {
            format,
            data,
            alpha,
            video_mode,
        } => (*format, data, alpha.as_ref(), *video_mode),
        // Already software-decoded — the still-image path produces these.
        PixelData::Rgba { data, stride } => {
            return Ok(DecodedFrame {
                pixels: PixelData::Rgba {
                    data: data.clone(),
                    stride: *stride,
                },
                w,
                h,
                pts_sec: frame.pts_sec,
            })
        }
    };

    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    match format {
        HapTextureFormat::Bc1 => expand(data, w, h, 8, &mut out, bc1_block)?,
        HapTextureFormat::Bc3 | HapTextureFormat::Bc3YCoCg => {
            expand(data, w, h, 16, &mut out, bc3_block)?;
        }
        HapTextureFormat::Bc4 => expand(data, w, h, 8, &mut out, bc4_as_red)?,
        HapTextureFormat::Bc7 => return Err(SoftDecErr::Unsupported(format)),
    }

    // Now the part `video()` would have done on the GPU. Same branches, same
    // constants, same clamp — see the module docs.
    match video_mode {
        1 | 2 => {
            unswizzle_ycocg(&mut out);
            if video_mode == 2 {
                let plane = alpha.ok_or(SoftDecErr::AlphaTruncated { need: 1, got: 0 })?;
                let mut a = vec![0u8; (w as usize) * (h as usize) * 4];
                expand(plane, w, h, 8, &mut a, bc4_as_red).map_err(|e| match e {
                    SoftDecErr::Truncated { need, got } => {
                        SoftDecErr::AlphaTruncated { need, got }
                    }
                    other => other,
                })?;
                for (px, ap) in out.as_chunks_mut::<4>().0.iter_mut().zip(a.as_chunks::<4>().0) {
                    px[3] = ap[0];
                }
            }
        }
        // Alpha-only: white, carrying the sampled value as alpha.
        3 => {
            for px in out.as_chunks_mut::<4>().0 {
                px[3] = px[0];
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
            }
        }
        _ => {}
    }

    Ok(DecodedFrame {
        pixels: PixelData::Rgba {
            data: out,
            stride: w * 4,
        },
        w,
        h,
        pts_sec: frame.pts_sec,
    })
}

/// Walk the block grid, decode each block, and scatter its 16 texels into the
/// RGBA image — cropping the block grid's padding, which is why the last block
/// of a non-multiple-of-4 image cannot simply be memcpy'd.
fn expand(
    data: &[u8],
    w: u32,
    h: u32,
    block_bytes: usize,
    out: &mut [u8],
    decode: fn(&[u8]) -> [[u8; 4]; 16],
) -> Result<(), SoftDecErr> {
    let (bx, by) = block_grid(w, h);
    let need = bx * by * block_bytes;
    if data.len() < need {
        return Err(SoftDecErr::Truncated {
            need,
            got: data.len(),
        });
    }
    let (w, h) = (w as usize, h as usize);
    for byi in 0..by {
        for bxi in 0..bx {
            let off = (byi * bx + bxi) * block_bytes;
            let texels = decode(&data[off..off + block_bytes]);
            for ty in 0..4 {
                let y = byi * 4 + ty;
                if y >= h {
                    break;
                }
                let row = (y * w) * 4;
                for tx in 0..4 {
                    let x = bxi * 4 + tx;
                    if x >= w {
                        break;
                    }
                    let p = row + x * 4;
                    out[p..p + 4].copy_from_slice(&texels[ty * 4 + tx]);
                }
            }
        }
    }
    Ok(())
}

/// 5- and 6-bit channels widen by replicating their high bits, which is what
/// the hardware does and what makes 0x1f/0x3f land on exactly 255.
const fn r5(v: u16) -> u8 {
    let v = (v & 0x1f) as u8;
    (v << 3) | (v >> 2)
}
const fn g6(v: u16) -> u8 {
    let v = (v & 0x3f) as u8;
    (v << 2) | (v >> 4)
}

fn rgb565(c: u16) -> [u8; 3] {
    [r5(c >> 11), g6(c >> 5), r5(c)]
}

fn lerp(a: u8, b: u8, num: u32, den: u32) -> u8 {
    let (a, b) = (u32::from(a), u32::from(b));
    ((a * (den - num) + b * num + den / 2) / den) as u8
}

fn lerp3(a: [u8; 3], b: [u8; 3], num: u32, den: u32) -> [u8; 3] {
    [
        lerp(a[0], b[0], num, den),
        lerp(a[1], b[1], num, den),
        lerp(a[2], b[2], num, den),
    ]
}

/// The 8-byte DXT1 colour block. `punch_through` is what distinguishes a
/// standalone BC1 block from the colour half of a BC3 one: in BC3 the
/// `c0 <= c1` encoding is *not* a transparency mode, the block is always read
/// as four interpolated colours.
fn colour_block(src: &[u8], punch_through: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([src[0], src[1]]);
    let c1 = u16::from_le_bytes([src[2], src[3]]);
    let (e0, e1) = (rgb565(c0), rgb565(c1));

    let mut pal = [[0u8; 4]; 4];
    pal[0] = [e0[0], e0[1], e0[2], 255];
    pal[1] = [e1[0], e1[1], e1[2], 255];
    if !punch_through || c0 > c1 {
        let a = lerp3(e0, e1, 1, 3);
        let b = lerp3(e0, e1, 2, 3);
        pal[2] = [a[0], a[1], a[2], 255];
        pal[3] = [b[0], b[1], b[2], 255];
    } else {
        let m = lerp3(e0, e1, 1, 2);
        pal[2] = [m[0], m[1], m[2], 255];
        pal[3] = [0, 0, 0, 0];
    }

    let idx = u32::from_le_bytes([src[4], src[5], src[6], src[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((idx >> (2 * i)) & 3) as usize];
    }
    out
}

fn bc1_block(src: &[u8]) -> [[u8; 4]; 16] {
    colour_block(src, true)
}

/// The 8-byte BC4 block, as 16 single-channel values.
fn bc4_values(src: &[u8]) -> [u8; 16] {
    let (r0, r1) = (src[0], src[1]);
    let mut pal = [0u8; 8];
    pal[0] = r0;
    pal[1] = r1;
    if r0 > r1 {
        for (i, p) in pal.iter_mut().enumerate().take(8).skip(2) {
            *p = lerp(r0, r1, i as u32 - 1, 7);
        }
    } else {
        for (i, p) in pal.iter_mut().enumerate().take(6).skip(2) {
            *p = lerp(r0, r1, i as u32 - 1, 5);
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    // Six bytes of 3-bit indices, little-endian across the whole run.
    let bits = u64::from_le_bytes([src[2], src[3], src[4], src[5], src[6], src[7], 0, 0]);
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// BC4 standing alone: the value lands in R, which is what the `videoMode == 3`
/// branch then reads.
fn bc4_as_red(src: &[u8]) -> [[u8; 4]; 16] {
    let v = bc4_values(src);
    let mut out = [[0u8, 0, 0, 255]; 16];
    for (o, r) in out.iter_mut().zip(v) {
        o[0] = r;
    }
    out
}

/// BC3: an 8-byte BC4 alpha block followed by an 8-byte always-four-colour
/// DXT1 block.
fn bc3_block(src: &[u8]) -> [[u8; 4]; 16] {
    let a = bc4_values(&src[0..8]);
    let mut out = colour_block(&src[8..16], false);
    for (o, av) in out.iter_mut().zip(a) {
        o[3] = av;
    }
    out
}

/// Scaled YCoCg-DXT5 (van Waveren & Castano), in place.
///
/// Byte-for-byte the arithmetic in `preamble.frag`'s `video()`: DXT5 holds
/// `(Co, Cg, scale, Y)` and the offset is 128/255, spelled there as
/// `0.501960784`.
fn unswizzle_ycocg(px: &mut [u8]) {
    // 128/255, spelled as the division so it is exactly the constant the
    // shader's literal `0.501960784` rounds to rather than a transcription.
    const OFFSET: f32 = 128.0 / 255.0;
    for c in px.as_chunks_mut::<4>().0 {
        let (r, g, b, a) = (
            f32::from(c[0]) / 255.0,
            f32::from(c[1]) / 255.0,
            f32::from(c[2]) / 255.0,
            f32::from(c[3]) / 255.0,
        );
        let scale = b.mul_add(255.0 / 8.0, 1.0);
        let co = (r - OFFSET) / scale;
        let cg = (g - OFFSET) / scale;
        let y = a;
        let rgb = [y + co - cg, y + cg, y - co - cg];
        for (o, v) in c.iter_mut().zip(rgb) {
            *o = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        c[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same aliasing trick as everywhere else in this crate: under wasm32 these
    // run in V8 through `wasm-bindgen-test`, unmodified. That matters more here
    // than usual — this code path only ever runs in a browser.
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// One BC1 block: two RGB565 endpoints and 16 two-bit indices.
    fn bc1(c0: u16, c1: u16, idx: [u8; 16]) -> Vec<u8> {
        let mut bits = 0u32;
        for (i, &v) in idx.iter().enumerate() {
            bits |= u32::from(v & 3) << (2 * i);
        }
        let mut b = Vec::new();
        b.extend_from_slice(&c0.to_le_bytes());
        b.extend_from_slice(&c1.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b
    }

    /// One BC4 block: two endpoints and 16 three-bit indices.
    fn bc4(r0: u8, r1: u8, idx: [u8; 16]) -> Vec<u8> {
        let mut bits = 0u64;
        for (i, &v) in idx.iter().enumerate() {
            bits |= u64::from(v & 7) << (3 * i);
        }
        let mut b = vec![r0, r1];
        b.extend_from_slice(&bits.to_le_bytes()[..6]);
        b
    }

    fn frame(format: HapTextureFormat, data: Vec<u8>, video_mode: i32) -> DecodedFrame {
        DecodedFrame {
            pixels: PixelData::Bc {
                format,
                data,
                alpha: None,
                video_mode,
            },
            w: 4,
            h: 4,
            pts_sec: 0.0,
        }
    }

    fn rgba_of(f: &DecodedFrame) -> Vec<u8> {
        let PixelData::Rgba { data, .. } = &f.pixels else {
            panic!("not decompressed to RGBA");
        };
        data.clone()
    }

    /// Red in RGB565 is 0xF800, green 0x07E0, blue 0x001F, white 0xFFFF.
    #[test]
    fn endpoints_decode_to_their_own_colours() {
        // c0 > c1, so four-colour mode; indices 0 and 1 must be the endpoints
        // exactly, with no interpolation rounding in the way.
        let mut idx = [0u8; 16];
        idx[1] = 1;
        let out = rgba_of(&to_rgba(&frame(HapTextureFormat::Bc1, bc1(0xF800, 0x001F, idx), 0)).unwrap());
        assert_eq!(&out[0..4], &[255, 0, 0, 255], "index 0 is endpoint 0");
        assert_eq!(&out[4..8], &[0, 0, 255, 255], "index 1 is endpoint 1");
    }

    #[test]
    fn five_and_six_bit_channels_widen_to_full_range() {
        // 0x1f/0x3f must reach 255, not 248/252 — a plain shift leaves white
        // slightly grey and every clip washed out by a hair.
        assert_eq!(rgb565(0xFFFF), [255, 255, 255]);
        assert_eq!(rgb565(0x0000), [0, 0, 0]);
    }

    #[test]
    fn four_colour_mode_interpolates_at_one_and_two_thirds() {
        let mut idx = [0u8; 16];
        idx[0] = 2;
        idx[1] = 3;
        // Black to white, so the interpolants are pure fractions of 255.
        let out = rgba_of(&to_rgba(&frame(HapTextureFormat::Bc1, bc1(0xFFFF, 0x0000, idx), 0)).unwrap());
        // color_2 = (2*c0 + c1)/3, so index 2 sits nearer c0 (white), not nearer c1.
        assert_eq!(out[0], 170, "index 2 is 2/3 of the way toward white");
        assert_eq!(out[4], 85, "index 3 is 1/3 of the way toward white");
        assert_eq!(out[3], 255, "four-colour mode is opaque");
    }

    #[test]
    fn three_colour_mode_index_three_is_transparent() {
        // c0 <= c1 selects the punch-through encoding, and index 3 there is
        // transparent black rather than a fourth colour.
        let mut idx = [0u8; 16];
        idx[0] = 3;
        idx[1] = 2;
        let out = rgba_of(&to_rgba(&frame(HapTextureFormat::Bc1, bc1(0x0000, 0xFFFF, idx), 0)).unwrap());
        assert_eq!(&out[0..4], &[0, 0, 0, 0], "index 3 punches through");
        assert_eq!(out[4 + 3], 255, "index 2 is still opaque");
        assert_eq!(out[4], 128, "index 2 is the midpoint");
    }

    #[test]
    fn a_bc3_colour_block_never_punches_through() {
        // The identical endpoint ordering that means "transparent" in BC1 is
        // just a four-colour block in BC3; alpha comes from the alpha block.
        let mut idx = [0u8; 16];
        idx[0] = 3;
        let mut data = bc4(255, 255, [0u8; 16]); // constant alpha 255
        data.extend(bc1(0x0000, 0xFFFF, idx));
        let out = rgba_of(&to_rgba(&frame(HapTextureFormat::Bc3, data, 0)).unwrap());
        assert_eq!(out[3], 255, "alpha comes from the alpha block, not the index");
        assert_eq!(out[0], 170, "index 3 is 2/3 of the way, not transparent black");
    }

    #[test]
    fn bc4_endpoints_and_both_interpolation_modes() {
        // r0 > r1: eight interpolated values, no 0/255 specials.
        let mut idx = [0u8; 16];
        for (i, v) in idx.iter_mut().enumerate().take(8) {
            *v = u8::try_from(i).unwrap();
        }
        let v = bc4_values(&bc4(140, 0, idx));
        assert_eq!(v[0], 140);
        assert_eq!(v[1], 0);
        assert_eq!(v[2], 120, "6/7 of 140");
        assert_eq!(v[7], 20, "1/7 of 140");

        // r0 <= r1: six values plus hard 0 and 255 at indices 6 and 7.
        let v = bc4_values(&bc4(0, 140, idx));
        assert_eq!(v[0], 0);
        assert_eq!(v[1], 140);
        assert_eq!(v[6], 0, "index 6 is a hard zero");
        assert_eq!(v[7], 255, "index 7 is a hard 255");
    }

    #[test]
    fn alpha_only_becomes_white_carrying_the_value() {
        // `videoMode == 3` in the preamble: vec4(1,1,1, c.r).
        let mut idx = [0u8; 16];
        idx[1] = 1;
        let out = rgba_of(&to_rgba(&frame(HapTextureFormat::Bc4, bc4(200, 40, idx), 3)).unwrap());
        assert_eq!(&out[0..4], &[255, 255, 255, 200]);
        assert_eq!(&out[4..8], &[255, 255, 255, 40]);
    }

    #[test]
    fn ycocg_unswizzles_to_the_original_colour() {
        // Encode a known RGB the way HapQ does — scale 1 (the b channel at 0),
        // so Co/Cg are stored directly with the 128/255 offset — then check the
        // decode lands back on it. This is the branch that silently ruins a
        // clip's colour if it is skipped, because the pixels are all *there*.
        let (r, g, b) = (0.75f32, 0.25f32, 0.5f32);
        let (y, co, cg) = (
            0.25f32.mul_add(r, 0.5f32.mul_add(g, 0.25 * b)),
            (r - b) * 0.5,
            0.5f32.mul_add(g, -(0.25 * (r + b))),
        );
        let enc = |v: f32| ((v + 128.0 / 255.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        let mut px = vec![enc(co), enc(cg), 0, (y * 255.0).round() as u8];
        unswizzle_ycocg(&mut px);
        for (i, want) in [r, g, b].iter().enumerate() {
            let got = f32::from(px[i]) / 255.0;
            assert!(
                (got - want).abs() < 0.02,
                "channel {i}: got {got}, want {want}"
            );
        }
        assert_eq!(px[3], 255, "no alpha plane means opaque");
    }

    #[test]
    fn a_hapm_alpha_plane_lands_in_the_alpha_channel() {
        let mut idx = [0u8; 16];
        idx[1] = 1;
        let mut colour = bc4(255, 255, [0u8; 16]);
        colour.extend(bc1(0xFFFF, 0x0000, [0u8; 16]));
        let f = DecodedFrame {
            pixels: PixelData::Bc {
                format: HapTextureFormat::Bc3YCoCg,
                data: colour,
                alpha: Some(bc4(200, 40, idx)),
                video_mode: 2,
            },
            w: 4,
            h: 4,
            pts_sec: 0.0,
        };
        let out = rgba_of(&to_rgba(&f).unwrap());
        assert_eq!(out[3], 200, "first texel takes the alpha plane's endpoint 0");
        assert_eq!(out[7], 40, "second takes endpoint 1");
    }

    #[test]
    fn a_non_block_aligned_image_is_cropped_not_padded() {
        // 6x6 is a 2x2 block grid covering 8x8; the output must be 6x6 and the
        // rows must not pick up the padding column.
        let solid = bc1(0xF800, 0xF800, [0u8; 16]);
        let data: Vec<u8> = solid.iter().copied().cycle().take(solid.len() * 4).collect();
        let f = DecodedFrame {
            pixels: PixelData::Bc {
                format: HapTextureFormat::Bc1,
                data,
                alpha: None,
                video_mode: 0,
            },
            w: 6,
            h: 6,
            pts_sec: 0.0,
        };
        let out = to_rgba(&f).unwrap();
        let PixelData::Rgba { data, stride } = &out.pixels else {
            panic!("not RGBA");
        };
        assert_eq!(*stride, 24, "6 px at 4 bytes");
        assert_eq!(data.len(), 6 * 6 * 4);
        assert!(
            data.as_chunks::<4>().0.iter().all(|p| *p == [255, 0, 0, 255]),
            "every in-bounds texel is the block's colour"
        );
    }

    #[test]
    fn a_short_payload_is_an_error_not_a_panic() {
        // let-else rather than `unwrap_err`, which would need `DecodedFrame:
        // Debug` — a derive that would put a megabyte of pixels in a message.
        let Err(err) = to_rgba(&frame(HapTextureFormat::Bc1, vec![0; 4], 0)) else {
            panic!("a 4-byte block decoded");
        };
        assert_eq!(err, SoftDecErr::Truncated { need: 8, got: 4 });
    }

    #[test]
    fn bc7_names_itself_rather_than_showing_black() {
        let Err(err) = to_rgba(&frame(HapTextureFormat::Bc7, vec![0; 16], 0)) else {
            panic!("BC7 decoded");
        };
        assert_eq!(err, SoftDecErr::Unsupported(HapTextureFormat::Bc7));
        assert!(err.to_string().contains("texture-compression-bc"));
    }

    #[test]
    fn an_already_rgba_frame_passes_through() {
        let f = DecodedFrame {
            pixels: PixelData::Rgba {
                data: vec![1, 2, 3, 4],
                stride: 4,
            },
            w: 1,
            h: 1,
            pts_sec: 1.5,
        };
        let out = to_rgba(&f).unwrap();
        assert_eq!(rgba_of(&out), vec![1, 2, 3, 4]);
        assert!((out.pts_sec - 1.5).abs() < f64::EPSILON);
    }

    /// The end-to-end claim this module exists for: a clip the baker actually
    /// wrote, walked by `Clip`, comes out as uploadable RGBA with the colours
    /// that were baked in. Everything above tests a block; this tests the path.
    #[test]
    fn a_baked_clip_decompresses_to_the_colour_it_was_baked_with() {
        use crate::clip::Clip;
        use vidiotic_bake::{hap, mov};

        const W: u32 = 8;
        const H: u32 = 8;
        // Four blocks of solid red, as `transcode.rs` would emit for a red frame.
        let block = bc1(0xF800, 0xF800, [0u8; 16]);
        let bc: Vec<u8> = block.iter().copied().cycle().take(block.len() * 4).collect();

        let mut w = mov::MovWriter::new(std::io::Cursor::new(Vec::new()), W, H, 30_000, 1_000)
            .expect("writer");
        w.write_sample(&hap::encode_hap1_frame(&bc), 0).expect("write");
        let bytes = std::rc::Rc::new(w.finish().expect("finish").into_inner());
        let mut clip = Clip::open(bytes).expect("open");

        let out = to_rgba(&clip.frame(0).expect("decode")).expect("decompress");
        let PixelData::Rgba { data, stride } = &out.pixels else {
            panic!("still block-compressed");
        };
        assert_eq!(*stride, W * 4);
        assert_eq!(data.len(), (W * H * 4) as usize);
        assert!(
            data.as_chunks::<4>().0.iter().all(|p| *p == [255, 0, 0, 255]),
            "the whole frame should be the red it was baked with"
        );
    }
}
