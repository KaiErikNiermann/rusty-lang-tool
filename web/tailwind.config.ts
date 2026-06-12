import type { Config } from "tailwindcss";

// Repo convention: Inter for UI, IBM Plex Sans for body, system fallback. Squiggle/severity colors
// live as theme tokens so the editor theme + UI stay in sync.
export default {
  content: ["./src/**/*.{html,svelte,ts}"],
  theme: {
    extend: {
      fontFamily: {
        sans: ["Inter", "system-ui", "-apple-system", "sans-serif"],
        body: ["IBM Plex Sans", "system-ui", "sans-serif"],
        mono: ["IBM Plex Mono", "ui-monospace", "monospace"],
      },
      colors: {
        // Diagnostic source palette (also referenced by the Monaco theme).
        spelling: "#e5484d",
        grammar: "#3b82f6",
        statistical: "#a855f7",
        neural: "#10b981",
      },
    },
  },
  plugins: [],
} satisfies Config;
