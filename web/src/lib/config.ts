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

/** A short error-laden sample per language, so the demo shows findings immediately. */
export const SAMPLE_TEXT: Record<string, string> = {
  en: "Your going to recieve to many msgs. I should of checked there email yesterday.",
  de: "Ich habe ein Apfel gegessen. Das ist nicht richtig geshrieben.",
  fr: "Je suis aller au marché. Il a manger une pomme.",
  es: "Ayer fui a la tienda y compre pan. No se como hacerlo.",
  it: "Ieri o andato al mercato. Non so come si fà.",
  ru: "Я пошёл в магазин и купил хлеб. Это не правильно написано.",
  ar: "ذهبت إلى المدرسه أمس. هذا ليس صحيحا.",
};
