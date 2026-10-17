import type { Metadata, Viewport } from "next";
import { Courier_Prime, EB_Garamond, IM_Fell_English, IM_Fell_English_SC, UnifrakturMaguntia } from "next/font/google";
import type { ReactNode } from "react";
import { Haunt, MotionToggle } from "@/components/Haunt";
import { Header } from "@/components/Header";
import { Ornament } from "@/components/Ornament";
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

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${fell.variable} ${fellSc.variable} ${garamond.variable} ${courier.variable} ${fraktur.variable}`}>
      <body>
        <Haunt />
        <WalletProvider>
          <Header />
          {children}
          <footer className="imprint">
            <Ornament kind="bat" />
            <span className="caps">Quantum-safe ML-DSA-65 keys · Unlocked by passkeys</span>
            <span>Not audited. For test chains only.</span>
            <MotionToggle />
          </footer>
        </WalletProvider>
      </body>
    </html>
  );
}
