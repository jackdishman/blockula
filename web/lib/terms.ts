"use client";

import { useCallback, useEffect, useState } from "react";
import { TERMS_KEY, TERMS_VERSION } from "./legal";

export { TERMS_KEY, TERMS_VERSION };

export interface Agreement {
  v: 1;
  /** The TERMS_VERSION agreed to. */
  version: string;
  /** When, as an ISO date-time on this device's clock. */
  at: string;
}

export function readAgreement(): Agreement | null {
  try {
    const a = JSON.parse(localStorage.getItem(TERMS_KEY) ?? "null") as Agreement | null;
    return a && a.v === 1 && typeof a.version === "string" && typeof a.at === "string" ? a : null;
  } catch {
    return null;
  }
}

/**
 * Whether this device agreed to the current terms: `null` until it has been read (render nothing
 * yet), then true or false. `agree()` records it; storage that refuses still lets this tab go on.
 */
export function useTerms(): { agreed: boolean | null; agree: () => void } {
  const [agreed, setAgreed] = useState<boolean | null>(null);
  useEffect(() => setAgreed(readAgreement()?.version === TERMS_VERSION), []);
  const agree = useCallback(() => {
    try {
      localStorage.setItem(TERMS_KEY, JSON.stringify({ v: 1, version: TERMS_VERSION, at: new Date().toISOString() } satisfies Agreement));
    } catch {
      // A private window or blocked storage: agreed for this tab only, asked again next visit.
    }
    setAgreed(true);
  }, []);
  return { agreed, agree };
}
