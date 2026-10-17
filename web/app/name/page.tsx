"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { RequireWallet } from "@/components/Gate";
import { Apparition } from "@/components/Haunt";
import { epitaph, NameClaim } from "@/components/NameClaim";
import { Tip } from "@/components/ui";

export default function NamePage() {
  const router = useRouter();
  const [typed, setTyped] = useState("");
  return (
    <RequireWallet>
      <main className="narrow stack-lg">
        <Link href="/" className="back">← Home</Link>
        <Apparition kind="grave" text={epitaph(typed)} />
        <h1>Claim a .boo name</h1>
        <p className="dim">Pick something friends can remember. Names are first come, first served.</p>
        <section className="card">
          <NameClaim onName={setTyped} onClaimed={() => router.push("/")} />
        </section>
        <Tip>A name is recorded on-chain and points to your address, so anyone, anywhere, can send to it. Your first name becomes how you show up in everyone&apos;s activity.</Tip>
      </main>
    </RequireWallet>
  );
}
