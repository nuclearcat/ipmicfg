# Releasing ipmicfg

GitHub Actions builds x86_64 packages in Ubuntu 26.04 LTS and Fedora 44
containers. Pull requests and pushes to `main` validate both packages; a version
tag builds them again, creates checksums, and publishes a GitHub release.

## Prepare the release

1. Start from a clean, up-to-date `main` branch.
2. Choose a semantic version without a leading `v`, for example `0.2.0`.
3. Update `version` in `Cargo.toml` and refresh the lockfile:

   ```sh
   cargo check
   ```

4. Review user-facing documentation and the roadmap. Summarize notable changes
   in the eventual GitHub release notes if the generated notes need editing.
5. Run the same source checks expected before packaging:

   ```sh
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test --locked
   cargo build --release --locked
   ```

6. Commit the version change, open a pull request, and wait for the `Packages`
   workflow to pass. Merge the pull request before tagging.

## Publish

Create an annotated tag whose value exactly matches `v` plus the version in
`Cargo.toml`, then push it:

```sh
git switch main
git pull --ff-only
git tag -a v0.2.0 -m "ipmicfg 0.2.0"
git push origin v0.2.0
```

The `Release` workflow will:

1. Reject a tag that does not match `Cargo.toml`.
2. Test and build an Ubuntu `.deb` and Fedora `.rpm`.
3. Inspect the package metadata and contents.
4. Create `SHA256SUMS`.
5. Create the GitHub release with generated notes and attach all three files.

After it completes, download both packages from the release and smoke-test them
on representative Ubuntu and Fedora hosts:

```sh
# Ubuntu
sudo apt install ./ipmicfg_*_amd64.deb

# Fedora
sudo dnf install ./ipmicfg-*.x86_64.rpm

ipmicfg --version
```

Edit the generated release notes if they need a clearer migration warning or
hardware-specific caveat. Packages are currently unsigned; users should verify
the published SHA-256 checksums.

## Recover from a failed release

If packaging fails, fix the problem on `main`, delete the remote and local tag,
and recreate it on the corrected commit. Do not move a tag after users may have
downloaded its artifacts; publish a patch release instead.

```sh
git push --delete origin v0.2.0
git tag --delete v0.2.0
```

If the workflow created a partial GitHub release, delete that draft/release in
GitHub before recreating the tag.

## Maintaining the packaging targets

The distro pins are intentional. When a new Ubuntu LTS or Fedora release becomes
the supported target:

1. Update the container image and artifact name in `packages.yml`.
2. Update the supported versions in `README.md` and this guide.
3. Check for newer pinned `cargo-deb` and `cargo-generate-rpm` versions.
4. Open a pull request and verify that both package jobs pass before merging.

Ubuntu LTS releases receive five years of standard maintenance. Fedora releases
are supported for about thirteen months, so expect to refresh the Fedora pin
roughly twice per year.
