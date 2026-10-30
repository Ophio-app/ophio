// SPDX-License-Identifier: Apache-2.0
import { Store } from "../../state/store";
export interface VoicePreferences {
  language: string;
  keepAudio: boolean;
  explained: boolean;
}
const defaults: VoicePreferences = {
  language: "",
  keepAudio: false,
  explained: false,
};
const stores = new Map<string, Store<VoicePreferences>>();
export function voicePreferences(profileId: string): Store<VoicePreferences> {
  let store = stores.get(profileId);
  if (store) return store;
  let initial = defaults;
  try {
    const value = JSON.parse(
      localStorage.getItem(`voice.${profileId}`) ?? "null",
    ) as Partial<VoicePreferences> | null;
    if (value)
      initial = {
        language: typeof value.language === "string" ? value.language : "",
        keepAudio: value.keepAudio === true,
        explained: value.explained === true,
      };
  } catch {
    /* Defaults remain usable when storage is unavailable. */
  }
  store = new Store(initial);
  stores.set(profileId, store);
  return store;
}
export function saveVoicePreferences(
  profileId: string,
  patch: Partial<VoicePreferences>,
): void {
  const store = voicePreferences(profileId),
    next = { ...store.get(), ...patch };
  localStorage.setItem(`voice.${profileId}`, JSON.stringify(next));
  store.set(next);
}
