import { base } from "$app/paths";

/** The integrity manifest baked into the deployed site (root of trust). */
export const MANIFEST_URL = `${base}/web-artifacts.json`;

/**
 * Where the compressed `.gz` artifacts live. In production this is the GitHub Release download URL
 * (passed at build via `VITE_ARTIFACT_BASE_URL`); locally it falls back to a static server you point
 * at `dist/web-artifacts/` (see README), or the site's own `/artifacts` dir.
 */
export const ARTIFACT_BASE_URL = import.meta.env.VITE_ARTIFACT_BASE_URL ?? `${base}/artifacts`;

/** The language selected on first load (lightest meaningful default). */
export const DEFAULT_LANG = "en";

// A short sample per language, chosen so the deployed L1+L2 path (no L3 confusion / L4 neural, which
// aren't in the released artifacts) flags at least one issue immediately — mostly clear misspellings,
// the most reliable cross-language signal.
export const SAMPLE_TEXT: Record<string, string> = {
  en: "Your going to recieve to many msgs. I should of checked there email yesterday.",
  de: "Ich habe ein Apfel gegessen. Das ist nicht richtig geshrieben.",
  fr: "Je voudrais un caffé et du suchre pour le petit déjeuner.",
  es: "Ayer comí una mazana muy delisiosa en el desayuno.",
  it: "Oggi ho mangato una pizza buonissma con gli amici.",
  ru: "Сегодня я купил хлеп и малако в магазине.",
  ar: "ذهبت إلى المدرسه أمس. هذا ليس صحيحا.",
};
