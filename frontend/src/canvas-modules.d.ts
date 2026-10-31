// Upstream's canvases are plain JavaScript (src/upstream/js).
declare module '*canvas/deploy/index.js' {
  export function initializeDeployCanvas(): void;
}
declare module '*canvas/environment/index.js' {
  export function initializeEnvironmentCanvas(): void;
}
