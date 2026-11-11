import { WebviewWindow } from '@tauri-apps/api/webviewWindow';

let opening: Promise<void> | null = null;
export function openQuickViewWindow(): Promise<void> {
  if (opening) return opening;
  opening = (async () => {
    const existing = await WebviewWindow.getByLabel('quickview');
    if (existing) {
      await existing.unminimize();
      await existing.show();
      await existing.setFocus();
      return;
    }
    await new Promise<void>((resolve, reject) => {
      const win = new WebviewWindow('quickview', {
        url: '/quick-view', title: 'LAP · Quick view', width: 1100, height: 760,
        minWidth: 600, minHeight: 420, dragDropEnabled: false, visible: false,
      });
      void win.once('tauri://created', () => resolve());
      void win.once('tauri://error', event => reject(new Error(String(event.payload))));
    });
  })().finally(() => { opening = null; });
  return opening;
}
