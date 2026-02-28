import semver from "semver";

export type BumpType = "patch" | "minor" | "major";

export function bumpVersion(current: string, bump: BumpType): string {
  const next = semver.inc(current, bump);
  if (!next) {
    throw new Error(`Invalid version "${current}" — cannot apply ${bump} bump`);
  }
  return next;
}
