<!-- Keep the PR small and legible. Explain the what and the why below. -->

## What & why

<!-- The change, and the domain reason for it. If it fixes a bug, say which
     kind (test / observation / model / pipeline) and where the data first went
     wrong. -->

## Checklist

- [ ] `just gate` is green (clippy `-D warnings` clean + daily tests pass).
- [ ] No new `#[ignore]`d tests, or a reason is stated in the code.
- [ ] Tests encode the domain rule, not the current output.
- [ ] Docs updated if behavior or a public surface changed.
- [ ] Any DOCX/OOXML conformance claim cites its spec section (ECMA-376 /
      ISO 29500, or the OPC part).
