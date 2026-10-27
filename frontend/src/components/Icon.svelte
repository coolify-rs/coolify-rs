<script lang="ts">
  // Outline icons, 16px, 1.5px stroke (DESIGN.md: sidebar navigation).
  let { name, size = 16 }: { name: string; size?: number } = $props();

  const paths: Record<string, string> = {
    folder: 'M3 7.5A1.5 1.5 0 0 1 4.5 6h4l2 2h9A1.5 1.5 0 0 1 21 9.5v8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z',
    key: 'M15.5 7.5a3.5 3.5 0 1 1-3.36 4.5H10l-1.5 1.5L7 12l-1.5 1.5L4 12l2-2h6.14A3.5 3.5 0 0 1 15.5 7.5zM16 10.5h.01',
    users: 'M16 19v-1a4 4 0 0 0-4-4H7a4 4 0 0 0-4 4v1M9.5 10a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM21 19v-1a4 4 0 0 0-3-3.87M15.5 4.13a3 3 0 0 1 0 5.74',
    plus: 'M12 5v14M5 12h14',
    chevron: 'M9 6l6 6-6 6',
    logout: 'M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 17l5-5-5-5M15 12H3',
    copy: 'M9 9h10v10H9zM5 15V5h10',
    sun: 'M12 4V2M12 22v-2M4 12H2M22 12h-2M5.6 5.6 4.2 4.2M19.8 19.8l-1.4-1.4M5.6 18.4l-1.4 1.4M19.8 4.2l-1.4 1.4M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10z',
    moon: 'M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z',
    layers: 'M12 3l9 5-9 5-9-5 9-5zM3 13l9 5 9-5',
  };
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={paths[name] ?? ''} />
</svg>
