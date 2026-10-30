// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { toast } from "../../design/Toast";

export function useLocalText(key: string, fallback = "") {
  const [value, setValue] = useState(() => {
    try {
      return localStorage.getItem(key) ?? fallback;
    } catch {
      return fallback;
    }
  });
  return [
    value,
    (next: string) => {
      setValue(next);
      try {
        localStorage.setItem(key, next);
      } catch {
        toast(
          "This draft could not be saved on this device. Keep this screen open until it is sent.",
          { tone: "stop" },
        );
      }
    },
  ] as const;
}
