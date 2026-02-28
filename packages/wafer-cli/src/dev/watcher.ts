import { watch } from "chokidar";

export interface WatcherHandle {
  close(): Promise<void>;
}

export function watchProject(
  cwd: string,
  onChange: () => void,
  debounceMs: number = 300,
): WatcherHandle {
  let timer: ReturnType<typeof setTimeout> | null = null;

  const watcher = watch(["blocks/**/*", "wafer.json"], {
    cwd,
    ignoreInitial: true,
    ignored: ["node_modules", "dist"],
  });

  const debounced = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(onChange, debounceMs);
  };

  watcher.on("change", debounced);
  watcher.on("add", debounced);
  watcher.on("unlink", debounced);

  return {
    async close() {
      if (timer) clearTimeout(timer);
      await watcher.close();
    },
  };
}
