Subband is a pure-Rust JPEG 2000 codec for Second Life textures, forked from hayro-jpeg2000. Second Life textures are raw J2C codestreams from untrusted sources, decoded at reduced resolution for discard levels and cancelled when no longer needed. Subband serves that workload: `Image::from_codestream`, `target_resolution`, `strict`, cancellation and bounded memory.

Upstream only decodes. Subband adds an encoder that writes J2C codestreams for texture uploads, readable at every discard level by Subband and by other viewers' decoders.

Subband is a codec for Second Life textures, not a general-purpose JPEG 2000 library. It decodes only the texture profile: 8-bit unsigned samples with one to four components, no subsampling and zero offsets. A JP2 file is only unwrapped to find its codestream. Anything outside the profile fails with `ValidationError::Unsupported`. Follow the Second Life server's needs and limits, and improve on the classic viewer's codec behavior rather than copying it.

Whatever a texture codec owns belongs in Subband, not in the viewer: codestream validation and resource limits, discard-level sizes, output at the exact size of a discard level, and pixel packing. The viewer keeps viewer policy: which discard level to request, caching and GPU upload.

## Implementation

- Behavior comes from what ISO/IEC 15444-1 (and 15444-2, for the color spaces) mandates, or is stated as our own choice where the spec says nothing.
- Use other codecs to compare output, never to justify behavior. They get things wrong too.
- Never copy code from other JPEG 2000 implementations.

## Code

- Malformed or hostile input must never panic, hang, or use unbounded time or memory. Avoid superlinear algorithms on broken files.
- Comments: none by default. Add one only for a non-obvious *why*, in one line. No narrating a fix, no spec quotes above checks, nothing that only makes sense to someone who saw the diff.
- Doc comments go only on public API: one short line describing that item alone. Leave existing docs at their current length.
- Order items by importance: public items first, then `pub(crate)`, then private, and by importance within each group.

## Diverging from upstream

- Match upstream's code style in files you touch, and put new behavior in new modules where practical.
- Leave alone upstream code that Subband doesn't replace, and delete upstream code in the same change that makes it dead.
- Write the code instead of adding a dependency when writing it is cheap.

## Contributing upstream

- Contribute to `LaurenzV/hayro` (pull requests, issues, comments) only when the owner asks for that specific contribution, and only with changes that help hayro too.
- Paths and names differ: upstream keeps this crate under `hayro-jpeg2000/` as `hayro_jpeg2000`. Build an upstream contribution in a clone of `LaurenzV/hayro`, on a branch from its `main`, with only that change and no agent files.
- The owner writes all prose, and the pull request discloses LLM help.
- Upstream commits use `hayro-jpeg2000: <Imperative summary>`, and the PR title matches it with code identifiers in backticks. The PR body says what the problem was and how it's fixed, without validation steps or comparisons with other decoders.

## Commits and pull requests

These rules cover Subband's own history. Upstream contributions follow the section above.

- Commit with one Conventional Commit header and no body: `<type>: <subject>` or `<type>(<scope>): <subject>`. Include a scope only when it adds useful context.
- Name branches with a type prefix and the change (`feat/discard-level-decode`).
- Title pull requests with a Conventional Commit subject line, and use the title as the squash-merge commit subject. Start the body with the problem in plain prose, then give the solution in a sentence or two. End with a `Verification:` paragraph listing only checks that ran.
- No tags and no crates.io publishing without the owner's approval.

## Checks

Run tests in release mode. CI also checks docs, each feature, the MSRV and `no_std`; see [ci.yml](.github/workflows/ci.yml).

- `cargo fmt --all -- --check`
- `cargo clippy --release --all-targets -- -D warnings`
- `cargo nextest run --release --no-fail-fast`, plus `cargo test --release --doc` when doc examples change
- Test the encoder by decoding what it writes, with Subband and with an independent decoder such as OpenJPEG.
- Fuzz decoding with `cargo +nightly fuzz run decode --fuzz-dir fuzz` after changes to parsing or resource limits. Add each crash as a regression test.
- [tests/README.md](tests/README.md) covers reference images and where test streams come from.
