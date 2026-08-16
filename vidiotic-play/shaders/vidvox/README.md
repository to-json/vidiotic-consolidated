# Vendored Vidvox ISF filters

A curated subset of the [Vidvox ISF-Files](https://github.com/Vidvox/ISF-Files)
collection by Vidvox, MIT licensed — the upstream license text is vendored
verbatim in [LICENSE](LICENSE) as the MIT terms require. Chosen for the effect
families VJs reach for most: glitch, film/look, distortion, feedback, masking,
kaleidoscope/mirror, halftone, strobe, and audio-reactive warps. Only
single-`.fs` filters are vendored:

- files with a paired `.vs` vertex shader need custom vertex stages the
  vidiotic ISF transpiler doesn't run;
- transitions (a second image input) are excluded until the transpiler gets
  multi-image support — extra image inputs currently alias to the stage input;
- generators (no video input) are intentionally left out; they are planned as a
  separate cue type;
- a few filters with `IMPORTED` image assets (e.g. the v002 CRT masks) are
  omitted since they would need the PNGs shipped alongside.

Every file here is compile-tested against the transpiler by
`bundled_isf_shaders_compile` (src/shader.rs); a file that stops transpiling
should be fixed or dropped, not skipped.

Load one onto a cue with the chain editor's ISF picker, same as any other
`.fs`.
