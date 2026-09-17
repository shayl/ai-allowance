# Releasing AI Allowance

AI Allowance uses Semantic Versioning and publishes immutable Windows artifacts
to the private GitHub repository's Releases page.

## Version sources

Keep the same version in:

- `package.json`
- `package-lock.json`
- `src-tauri/Cargo.toml`
- `src-tauri/Cargo.lock` for the `ai-allowance` package
- `src-tauri/tauri.conf.json`

Release tags use the matching version with a `v` prefix. Version `0.2.0`
therefore uses tag `v0.2.0`.

## Release checklist

1. Choose the next version according to Semantic Versioning.
2. Update every version source listed above.
3. Move completed entries from `CHANGELOG.md`'s **Unreleased** section into a
   dated version section.
4. Run:

   ```powershell
   npm ci
   npm test
   npm run build
   cargo fmt --manifest-path src-tauri\Cargo.toml --check
   cargo test --manifest-path src-tauri\Cargo.toml
   ```

5. Commit the version change and push `main`.
6. Create and push the release tag:

   ```powershell
   git tag -a v0.2.0 -m "AI Allowance v0.2.0"
   git push origin v0.2.0
   ```

The `Release` GitHub Actions workflow builds on `windows-latest`, creates the
GitHub Release, generates release notes from the commit history, and uploads
the portable executable, MSI, and NSIS installer.

## Artifact retention

GitHub Releases are the permanent distribution record. Local files under
`src-tauri\target\release` are build output and are intentionally not committed
to Git.

Do not move an existing version tag or replace a published release. If a build
needs correction, publish a new patch version.
