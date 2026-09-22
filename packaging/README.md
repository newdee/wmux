# Package manifests

Generated, not hand-written: `tools\package-manifests.ps1 -Version X.Y.Z`
reads the two `.sha256` files and the MSI's ProductCode from the GitHub
release for `vX.Y.Z` and writes

- `winget\manifests\n\newdee\wmux\X.Y.Z\` — the three WinGet manifests
  (version, installer, defaultLocale), laid out as
  [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) wants
  them; `winget validate --manifest <that directory>` passes.
- `scoop\wmux.json` — a Scoop manifest for the zip, with `checkver` and
  `autoupdate` so a bucket keeps up with releases by itself.

Run it once per release, commit the output, and then:

**WinGet.** Copy `winget\manifests\n\newdee\wmux\X.Y.Z` into a fork of
winget-pkgs at the same path and open a pull request titled
`New package: newdee.wmux version X.Y.Z` (later versions: `New version:`).
The bot validates and installs it in a VM; a maintainer merges it, and
`winget install newdee.wmux` works a few hours later. Until then, the
manifests install locally with
`winget install --manifest packaging\winget\manifests\n\newdee\wmux\X.Y.Z`
(needs `winget settings` → `LocalManifestFiles` enabled, once).

**Scoop.** The manifest works straight from this repository:

```powershell
scoop install https://raw.githubusercontent.com/newdee/wmux/master/packaging/scoop/wmux.json
```

For `scoop install wmux` without a URL, open a pull request adding
`wmux.json` to the `bucket/` directory of
[ScoopInstaller/Extras](https://github.com/ScoopInstaller/Extras) (their
checks run `checkver` and `autoupdate` against it), or keep it in a bucket
of your own.
