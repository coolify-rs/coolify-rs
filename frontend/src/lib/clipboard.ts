// Port of upstream resources/js/clipboard.js.
//
// Copies text that can still be loading (for example logs from a request).
// WebKit (Safari and every iOS browser) accepts a clipboard write only while the
// click is still active, so an await before the write fails there. Giving
// ClipboardItem the pending Promise starts the write inside the click.
// https://webkit.org/blog/10855/async-clipboard-api/

type ClipboardLike = Pick<Clipboard, 'writeText'> & Partial<Pick<Clipboard, 'write'>>;
type ClipboardItemLike = new (items: Record<string, PromiseLike<Blob>>) => ClipboardItem;

export async function copyTextToClipboard(
  text: string | PromiseLike<string>,
  clipboard: ClipboardLike | undefined = globalThis.navigator?.clipboard,
  Item: ClipboardItemLike | undefined = globalThis.ClipboardItem,
): Promise<void> {
  const textPromise = Promise.resolve(text);
  if (clipboard?.write && Item) {
    try {
      const blob = textPromise.then((value) => new Blob([value], { type: 'text/plain' }));
      await clipboard.write([new Item({ 'text/plain': blob })]);
      return;
    } catch {
      // Rethrows when the text failed to load. Otherwise try writeText for
      // browsers that do not accept a Promise in ClipboardItem.
      await textPromise;
    }
  }
  if (!clipboard) throw new Error('Clipboard is not available.');
  await clipboard.writeText(await textPromise);
}
