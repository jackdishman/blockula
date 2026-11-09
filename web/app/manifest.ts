import type { MetadataRoute } from "next";
import { SITE } from "@/lib/config";

// A static file in every build, the served export included (no server renders it there).
export const dynamic = "force-static";

/**
 * `manifest.webmanifest`: what Add to Home Screen (iOS), Add to Dock (macOS Safari) and installing
 * (Chrome) name and open. `start_url`, `scope` and the icons are relative to the manifest's own
 * URL, so the one file works at `/` (the hosted wallet) and at `/wallet/` (the wallet a node
 * serves). That node puts its chain's name in place of "Blockula" (node/src/wallet.rs), as it does
 * the pages' titles; colors are the paper and ink of app/globals.css.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: SITE ? "Blockula" : "Blockula Wallet",
    short_name: "Blockula",
    description: SITE ? "Launch a small blockchain for you and your friends." : "A post-quantum wallet unlocked by your passkey.",
    start_url: "./",
    scope: "./",
    display: "standalone",
    background_color: "#ece3cd",
    theme_color: "#ece3cd",
    icons: [
      { src: "icon-192.png", sizes: "192x192", type: "image/png" },
      { src: "icon-512.png", sizes: "512x512", type: "image/png" },
    ],
  };
}
