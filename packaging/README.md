# Package manifests

Generated, not hand-written: `tools\package-manifests.ps1 -Version X.Y.Z`
reads the `.sha256` files and the MSIs' ProductCodes from the GitHub release
for `vX.Y.Z` and writes

- `winget\manifests\n\newdee\keepane\X.Y.Z\` — the three WinGet manifests
  (version, installer, defaultLocale); the installer manifest lists the MSI
  for the machine (`Scope: machine`) and, from 0.15.2 on, the one for the
  user (`Scope: user`, no administrator rights). Laid out as
  [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) wants
  them; `winget validate --manifest <that directory>` passes.
- `scoop\keepane.json` — a Scoop manifest for the zip, with `checkver` and
  `autoupdate` so a bucket keeps up with releases by itself.

The release workflow runs it for every tag from the files it just built
(`-MsiPath`/`-UserMsiPath`/`-ZipPath`: no download), attaches `keepane-X.Y.Z-manifests.zip`
to the release and commits the output here, so this directory follows
releases by itself. Running it by hand is for a release made some other
way. Either way, then:

**WinGet.** The first version goes in by hand, from the `newdee/winget-pkgs`
fork: [microsoft/winget-pkgs#441466](https://github.com/microsoft/winget-pkgs/pull/441466)
(`New package: newdee.keepane`; an earlier one, #440225, was closed). Its
bot validates and installs it in a VM once the CLA is signed on the PR; a
maintainer merges it, and `winget install newdee.keepane` works a
few hours later. After that, every release can submit its own update:
add a repository secret `WINGET_TOKEN` (a classic token with `public_repo`
of the account owning the fork) and the release workflow runs
`wingetcreate update newdee.keepane --submit` for each tag. Without the
secret that step is skipped, and a failed submission never fails a release.
Until the package is in, the manifests install locally with
`winget install --manifest packaging\winget\manifests\n\newdee\keepane\X.Y.Z`
(needs `winget settings` → `LocalManifestFiles` enabled, once).

**Scoop.** The manifest works straight from this repository:

```powershell
scoop install https://raw.githubusercontent.com/newdee/keepane/master/packaging/scoop/keepane.json
```

For `scoop install keepane` without a URL, open a pull request adding
`keepane.json` to the `bucket/` directory of
[ScoopInstaller/Extras](https://github.com/ScoopInstaller/Extras) (their
checks run `checkver` and `autoupdate` against it), or keep it in a bucket
of your own.
