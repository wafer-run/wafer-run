import { execFile, type ExecFileOptions } from "node:child_process";

export interface ExecResult {
  stdout: string;
  stderr: string;
}

export function exec(
  cmd: string,
  args: string[],
  opts?: ExecFileOptions,
): Promise<ExecResult> {
  return new Promise((resolve, reject) => {
    execFile(cmd, args, { ...opts, maxBuffer: 10 * 1024 * 1024 }, (err, stdout, stderr) => {
      if (err) {
        const message = stderr?.toString().trim() || err.message;
        reject(new Error(`${cmd} ${args.join(" ")} failed: ${message}`));
      } else {
        resolve({
          stdout: stdout?.toString() ?? "",
          stderr: stderr?.toString() ?? "",
        });
      }
    });
  });
}
