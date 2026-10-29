# Changelog: wyrdline-hub chart

## Unreleased

- The chart's `version` and `appVersion` are stamped from the release tag
  (`scripts/release/stamp-chart.sh`), so a released chart's default image is
  the image that release publishes. The packaged chart is a release asset,
  `wyrdline-hub-<version>.tgz`, covered by the release checksums and signature.
- A chart built from the repository (`appVersion 0.0.0`) refuses to render
  without `image.tag` or `image.digest`, instead of pulling a tag that is never
  published.
- **`image.digest` is preferred in production.** A digest pins the exact image
  that was scanned and signed; a tag can be moved. Set it, or `image.tag` to a
  published version, for any install you keep.
