Subband is a pure-Rust JPEG 2000 codec for Second Life textures, for any Second Life viewer. It is forked from hayro-jpeg2000 and does not track it. Textures arrive as raw J2C codestreams from untrusted sources, are decoded at reduced resolution for discard levels, and are cancelled when the viewer drops them. Subband also writes J2C codestreams for texture uploads that every viewer's decoder reads at every discard level.

## Non-negotiables

1. **Fast**. Subband is the fastest decoder for Second Life textures. Measure every change on a decode path with the benches; parity with `main` is the floor.
2. **Hostile input**. Malformed, truncated and oversized streams must not panic, hang, or use unbounded time or memory. Avoid superlinear algorithms on broken files. Valid but unusual Second Life content still decodes.
3. **Simple**. Second Life textures are all Subband does: 8-bit unsigned samples, one to four components, no subsampling, zero offsets, and JP2 unwrapped to its codestream. Other input is unsupported. Add no option, field or branch for anything else: a value that can only be one thing is not a parameter.
4. **Any viewer**. Subband owns what a texture codec owns: codestream validation and resource limits, discard-level sizes, output at the exact size of a discard level, and pixel packing. Viewers keep viewer policy: which discard level to request, caching and GPU upload. No viewer's types, names or policy appear in Subband.

## Working style

- Do not preserve complexity because it exists. Do not add machinery because it looks impressive.
- YAGNI. Fight scope creep. Do what was asked, minimally, and add nothing creative.
- Reuse before writing. Extend an existing implementation instead of adding a parallel one.
- Replace, do not layer. Delete what your change makes dead in the same change.
- Write the code instead of adding a dependency when writing it is cheap.
- Do not change toolchain settings, CI workflows, lint, fuzz or bench setup without an explicit request.

## References

- Behavior comes from ISO/IEC 15444-1, or is stated as our own choice where the standard says nothing.
- The reference codecs are OpenJPEG, Grok, j2k and Kakadu. Compare output and speed with the first three; Kakadu is read from its documentation and never run. Never copy code from any of them: they inform a choice, they never justify one.
- Add a JPEG 2000 capability when Second Life content uses it or when it makes a viewer's texture work faster.

## Structure

- Keep each file focused on one cohesive responsibility. Split distinct responsibilities when justified; avoid fragmentation.
- A module with submodules is a directory with `mod.rs`; never place `foo.rs` beside `foo/`.
- Order Rust files as follows: module declarations, `use` declarations including re-exports, constants and statics, type aliases, types with their impls immediately following, free functions, then tests.
- Benches live in one Divan target: `benches/main.rs` declares every bench module. Function identifiers are benchmark names; keep them short, with no `bench_` prefix. Every fixture goes through `fixture!`.

## Code

- Minimize visibility. Use `unsafe` only when necessary.
- Propagate with `?`. `unwrap`, `expect`, indexing, slicing and unchecked arithmetic are for invariants you can prove, never for stream data.
- Handle every fallible result.
- Errors are `DecodeError`: `Unsupported`, `TooLarge`, `Malformed`, `Cancelled` or `BufferSize`. `Option<T>` for absence.
- Check limits before allocating. Decode straight into the caller's buffer, and allocate only for the resolution levels a decode needs.

## Naming

- Concise names; no redundant qualifiers in public APIs. Short locals are fine when context is clear.
- Import types and traits directly when their names are clear. Import the module for free functions (`mem::take`), and keep the module on generic names (`fmt::Result`).
- Outside `use` declarations, a path names at most one module: `mem::take`, never `std::mem::take` or `crate::j2c::Header`. Do not prefix child modules with `self::` in code.
- Put `use` only at module scope. Start every `use` path with `std`, `core`, a crate name, `crate`, `self` or `super`. Use `super` at most once; use `crate::` above the parent module.
- Use `as` only for a name collision that importing a module cannot resolve. `use Trait as _` is allowed.
- Keep JPEG 2000 marker and box names as the standard spells them.

## Comments

Source has no comments or doc comments. If code seems to need a comment to explain its behavior, improve its names, types or structure until the explanation is clear from the code.

## Testing

- Spend tests where bugs hide: hostile and truncated input, every limit, cancellation, prefix decode, discard-level sizes and bit-exact output against references.
- Test behavior through the public API. Extend an existing test or harness before adding a new one.
- Use property-based tests where inputs interact, such as prefixes of every fixture. Generators must be valid and failures reproducible.
- Wait on completion signals, never on sleeps.
- Use `assert_matches!` instead of `assert!(matches!(..))`.
- Keep test names short (`<subject>_<outcome>`), with no `test_` prefix.
- Test the encoder by decoding what it writes, with Subband and with an independent decoder such as OpenJPEG.
- Fuzz after changes to parsing or limits, and add each crash as a regression test.
- [tests/README.md](tests/README.md) covers the streams and reference images.

## Versioning

- SemVer. Bump at release boundaries, not for individual changes.
- No tags and no crates.io publishing without the owner's approval.

## Commits and pull requests

- Commit with one Conventional Commit header and no body: `<type>: <subject>` or `<type>(<scope>): <subject>`. Add `!` before the colon for breaking changes.
- Name branches with a type prefix and the change (`feat/discard-level-decode`).
- Title pull requests with a Conventional Commit subject line, and use the title as the squash-merge commit subject. Open the body with the problem in plain prose, then the solution in a sentence or two. End with a `Verification:` paragraph that reports only checks that ran.

## Checks

CI runs the tests on Windows, macOS and Linux, checks docs, each feature and unused dependencies, and runs the benches on CodSpeed; see [ci.yml](.github/workflows/ci.yml). [audit.yml](.github/workflows/audit.yml) checks advisories, licences and sources.

- `cargo fmt --all -- --check`
- `cargo clippy --release --all-targets -- -D warnings`
- `cargo nextest run --release --no-fail-fast`
- `cargo +nightly fuzz run decode --fuzz-dir fuzz` after changes to parsing or limits
- `cargo bench --bench main` for any change on a decode path, against `main`
