// Plain constants the legal pages (server components) and the agreement (client) share.

/**
 * The Terms of Use and Privacy Policy in force (app/terms, app/privacy). A new date asks everyone
 * to agree again: change it whenever either page changes what someone agrees to.
 */
export const TERMS_VERSION = "2026-11-08";

/** The project's repository (components/Landing.tsx REPO, which is a client module's). */
export const LEGAL_REPO = "https://github.com/jackdishman/blockula";

/** localStorage key of this device's agreement (lib/terms.ts): `{ v: 1, version, at }`, nothing else. */
export const TERMS_KEY = "blockula.terms";
