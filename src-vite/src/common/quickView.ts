export interface QuickViewFile { path: string; name: string; size: number }
export const QUICK_VIEW_LIMIT = 1000;

export function mergeQuickViewFiles(existing: QuickViewFile[], incoming: QuickViewFile[]) {
  const paths = new Set(existing.map(file => file.path));
  const files = [...existing];
  let truncated = false;
  for (const file of incoming) {
    if (paths.has(file.path)) continue;
    if (files.length >= QUICK_VIEW_LIMIT) { truncated = true; break; }
    paths.add(file.path);
    files.push(file);
  }
  return { files, truncated };
}

export function selectQuickViewFile(paths: string[], selected: Set<string>, path: string,
  anchor: string | null, toggle: boolean, range: boolean) {
  const next = toggle ? new Set(selected) : new Set<string>();
  const start = anchor ? paths.indexOf(anchor) : -1;
  const end = paths.indexOf(path);
  if (range && start >= 0 && end >= 0) {
    paths.slice(Math.min(start, end), Math.max(start, end) + 1).forEach(value => next.add(value));
  } else if (toggle && next.has(path)) next.delete(path);
  else next.add(path);
  return next;
}
