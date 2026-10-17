import type { Metadata, Viewport } from "next";
import { Courier_Prime, EB_Garamond, IM_Fell_English, IM_Fell_English_SC, UnifrakturMaguntia } from "next/font/google";
import { connection } from "next/server";
import type { ReactNode } from "react";
import { ChainGate } from "@/components/ChainGate";
import { Haunt, MotionToggle } from "@/components/Haunt";
import { Header } from "@/components/Header";
import { Ornament } from "@/components/Ornament";
import { IS_MAINNET_BUILD } from "@/lib/config";
import { WalletProvider } from "@/lib/wallet";
import "./globals.css";

// Same faces as the node's explorer (node/src/explorer.html): Fell for display, Garamond for
// reading, Courier for anything machine-made (addresses, key ids), Fraktur for the wordmark only.
const fell = IM_Fell_English({ weight: "400", style: ["normal", "italic"], subsets: ["latin"], variable: "--font-fell" });
const fellSc = IM_Fell_English_SC({ weight: "400", subsets: ["latin"], variable: "--font-fell-sc" });
const garamond = EB_Garamond({ subsets: ["latin"], style: ["normal", "italic"], variable: "--font-garamond" });
const courier = Courier_Prime({ weight: ["400", "700"], subsets: ["latin"], variable: "--font-courier" });
const fraktur = UnifrakturMaguntia({ weight: "400", subsets: ["latin"], variable: "--font-fraktur" });

export const metadata: Metadata = {
  title: "Blockula Wallet",
  description: "A post-quantum wallet unlocked by your passkey. No seed phrase, just a .boo name.",
};

export const viewport: Viewport = { themeColor: "#ece3cd", width: "device-width", initialScale: 1 };

export default async function RootLayout({ children }: { children: ReactNode }) {
  // Render per request: the CSP nonce (proxy.ts) is new on every response, so no page is static.
  await connection();
  return (
    <html lang="en" className={`${fell.variable} ${fellSc.variable} ${garamond.variable} ${courier.variable} ${fraktur.variable}`}>
      <body>
        <Haunt />
        <WalletProvider>
          <Header />
          <ChainGate>{children}</ChainGate>
          <footer className="imprint">
            <Ornament kind="bat" />
            <span className="caps">Quantum-safe ML-DSA-65 keys · Unlocked by passkeys</span>
            {/* Mainnet builds are for real money (after the audit, WAL-3); test builds say what they are. */}
            <span data-testid="imprint-note">{IS_MAINNET_BUILD ? "Self-custodial: your key is derived on this device and never leaves it." : "Not audited. For test chains only."}</span>
            <MotionToggle />
          </footer>
        </WalletProvider>
      </body>
    </html>
  );
}
