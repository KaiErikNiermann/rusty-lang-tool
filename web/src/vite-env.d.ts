/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Base URL of the compressed artifact host (the GitHub Release download dir in production). */
  readonly VITE_ARTIFACT_BASE_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
