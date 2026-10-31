<script lang="ts">
  // Port of upstream components/forms/monaco-editor.blade.php (used by
  // components/forms/textarea.blade.php with `useMonacoEditor`): Monaco's AMD
  // build, loaded on demand from /js/monaco-editor-{version}/min, with the
  // coolify-dark theme.
  /* eslint-disable @typescript-eslint/no-explicit-any */
  let {
    value = $bindable(''),
    language = '',
    readonly = false,
    rows = 0,
    placeholder = 'Start typing here',
  }: { value?: string; language?: string; readonly?: boolean; rows?: number; placeholder?: string } = $props();

  const monacoVersion = '0.52.2';
  let element: HTMLDivElement | undefined = $state();
  let editor: any = null;

  const w = window as any;

  function loadMonaco(): Promise<any> {
    if (w.monaco?.editor) return Promise.resolve(w.monaco);
    if (w.__coolifyMonacoPromise) return w.__coolifyMonacoPromise;
    w.__coolifyMonacoPromise = new Promise((resolve, reject) => {
      const ready = () => {
        w.require.config({ paths: { vs: `/js/monaco-editor-${monacoVersion}/min/vs` } });
        const proxy = URL.createObjectURL(
          new Blob(
            [
              `self.MonacoEnvironment={baseUrl:'${window.location.origin}/js/monaco-editor-${monacoVersion}/min'};importScripts('${window.location.origin}/js/monaco-editor-${monacoVersion}/min/vs/base/worker/workerMain.js');`,
            ],
            { type: 'text/javascript' },
          ),
        );
        w.MonacoEnvironment = { getWorkerUrl: () => proxy };
        w.require(['vs/editor/editor.main'], () => {
          if (!w.__coolifyMonacoThemeDefined) {
            w.monaco.editor.defineTheme('coolify-dark', {
              base: 'vs-dark',
              inherit: true,
              rules: [],
              colors: {
                'editor.background': '#0b0b0c',
                'editorGutter.background': '#0b0b0c',
                'editorStickyScroll.background': '#0b0b0c',
                'minimap.background': '#0b0b0c',
                'scrollbarSlider.background': '#ffffff1a',
                'scrollbarSlider.hoverBackground': '#ffffff2e',
                'scrollbarSlider.activeBackground': '#ffffff40',
                'scrollbar.shadow': '#00000000',
              },
            });
            w.__coolifyMonacoThemeDefined = true;
          }
          resolve(w.monaco);
        });
      };
      if (typeof w._amdLoaderGlobal !== 'undefined') return ready();
      const script = document.createElement('script');
      script.src = `/js/monaco-editor-${monacoVersion}/min/vs/loader.js`;
      script.onload = ready;
      script.onerror = () => {
        w.__coolifyMonacoPromise = null;
        reject(new Error('Monaco failed to load'));
      };
      document.head.appendChild(script);
    });
    return w.__coolifyMonacoPromise;
  }

  const isDark = () => document.documentElement.classList.contains('dark');

  $effect(() => {
    if (!element) return;
    let disposed = false;
    let observer: MutationObserver | null = null;
    loadMonaco().then((monaco) => {
      if (disposed || !element) return;
      editor = monaco.editor.create(element, {
        value: value ?? '',
        theme: isDark() ? 'coolify-dark' : 'vs',
        wordWrap: 'on',
        readOnly: readonly,
        minimap: { enabled: false },
        fontSize: '15px',
        lineNumbersMinChars: 3,
        automaticLayout: true,
        language,
        placeholder,
        domReadOnly: readonly,
        contextmenu: !readonly,
        renderLineHighlight: 'none',
        stickyScroll: { enabled: false },
        padding: { top: 12, bottom: 12 },
        overviewRulerLanes: 0,
        overviewRulerBorder: false,
        hideCursorInOverviewRuler: true,
        scrollbar: { vertical: 'auto', horizontal: 'auto', verticalScrollbarSize: 8, horizontalScrollbarSize: 8, useShadows: false },
      });
      editor.onDidChangeModelContent(() => {
        value = editor.getValue();
      });
      observer = new MutationObserver(() => monaco.editor.setTheme(isDark() ? 'coolify-dark' : 'vs'));
      observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
    });
    return () => {
      disposed = true;
      observer?.disconnect();
      editor?.dispose();
      editor = null;
    };
  });

  // `$watch('monacoContent')`: outside changes (reset, reload) replace the text.
  $effect(() => {
    const v = value ?? '';
    if (editor && editor.getValue() !== v) editor.setValue(v);
  });
  $effect(() => {
    editor?.updateOptions({ readOnly: readonly, domReadOnly: readonly });
  });
</script>

<div class="w-full" style={rows > 0 ? `--editor-height: ${rows * 23 + 38}px` : undefined}>
  <div class="coolify-monaco-editor flex-1">
    <div class="relative z-10 w-full h-full">
      <div
        bind:this={element}
        class="w-full text-md {readonly ? 'opacity-65' : ''}"
        style="height: var(--editor-height, calc(100vh - 20rem)); min-height: 150px;"
      ></div>
    </div>
  </div>
</div>
