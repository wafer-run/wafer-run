# Releasing wafer-run

## Version Scheme

wafer-run uses [Semantic Versioning](https://semver.org/): `MAJOR.MINOR.PATCH`

- **MAJOR** — breaking changes to block API, runtime API, or message format
- **MINOR** — new blocks, new features, new service traits
- **PATCH** — bug fixes, security patches, dependency updates

## Pre-Release Checklist

Before tagging a release, verify:

- [ ] `main` branch CI is green (check the [Actions tab](../../actions))
- [ ] Update `version` in `Cargo.toml` workspace section to match the intended release
- [ ] No known critical bugs (check [open issues](../../issues))
- [ ] Run the full suite locally, with `WAFER_CONFORMANCE_POSTGRES_URL` set
      so it includes the PostgreSQL step (see the header of `scripts/check.sh`):
  ```bash
  ./scripts/check.sh
  ```
- [ ] If this release changes block APIs or service traits, update the docs

## Creating a Release

There is no release workflow: a tag and its GitHub Release are made by
hand, and nothing is published to a registry.

```bash
# 1. Make sure you're on main and up to date
git checkout main
git pull

# 2. Tag the release
git tag v0.2.0

# 3. Push the tag
git push origin v0.2.0

# 4. Create the GitHub Release, with the CHANGELOG section as its notes
gh release create v0.2.0 --title v0.2.0 --notes-file <notes.md>
```

## After Release

- [ ] Verify the [GitHub Release](../../releases) was created
- [ ] Update downstream consumers' wafer-run dependency if needed

## Hotfix Process

Branch protection prevents pushing directly to `main` — hotfixes follow the same PR flow:

```bash
# 1. Create a hotfix branch
git checkout main && git pull
git checkout -b hotfix/v0.2.1

# 2. Fix the bug, commit, push
git push -u origin hotfix/v0.2.1

# 3. Open a PR — CI must pass, 1 approval required
gh pr create --title "fix: critical bug description"

# 4. After merge, tag and release the patch as in "Creating a Release"
git checkout main && git pull
git tag v0.2.1
git push origin v0.2.1
gh release create v0.2.1 --title v0.2.1 --notes-file <notes.md>
```

## Undoing a Release

If a release was tagged by mistake:

```bash
# Delete the tag locally and remotely
git tag -d v0.2.0
git push origin --delete v0.2.0
```

Then delete the GitHub Release from the [Releases page](../../releases).
