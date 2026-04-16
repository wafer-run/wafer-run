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
- [ ] Run tests locally:
  ```bash
  cargo test --workspace
  ```
- [ ] If this release changes block APIs or service traits, update the docs

## Creating a Release

```bash
# 1. Make sure you're on main and up to date
git checkout main
git pull

# 2. Tag the release
git tag v0.2.0

# 3. Push the tag — this triggers the release workflow
git push origin v0.2.0
```

The [Release workflow](../../actions/workflows/release.yml) will automatically:
1. Run the full test suite
2. Create a GitHub Release
3. Update block manifests in the [wafer-run/registry](https://github.com/wafer-run/registry) repo

## After Release

- [ ] Verify the [GitHub Release](../../releases) was created
- [ ] Verify the [registry repo](https://github.com/wafer-run/registry) was updated with new manifests
- [ ] Update solobase's wafer-run dependency if needed

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

# 4. After merge, tag the patch release
git checkout main && git pull
git tag v0.2.1
git push origin v0.2.1
```

## Undoing a Release

If a release was tagged by mistake:

```bash
# Delete the tag locally and remotely
git tag -d v0.2.0
git push origin --delete v0.2.0
```

Then delete the GitHub Release from the [Releases page](../../releases). Note: the registry manifests will still reference the deleted version — manually revert that commit in `wafer-run/registry` if needed.
