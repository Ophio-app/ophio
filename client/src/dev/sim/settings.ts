// SPDX-License-Identifier: Apache-2.0
import type {
  ChannelIdentity,
  Command,
  CommandResult,
  DeviceRecord,
  Grants,
  HarnessInfo,
  PendingChannelLink,
  PreviewEndpoint,
  Query,
  QueryData,
} from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import type { SimHost } from "./host";
import { ids } from "./scenario";
export function settingsFixtures(
  grants: Grants,
  now: number,
): {
  devices: DeviceRecord[];
  identities: ChannelIdentity[];
  links: Record<string, PendingChannelLink>;
  previews: PreviewEndpoint[];
} {
  const day = 86400000;
  return {
    devices: [
      {
        device_id: ids.device,
        name: "Pixel 9",
        platform: "Android 16",
        paired_at: now - 12 * day,
        last_seen_at: now,
        revoked_at: null,
        grants,
        is_current: true,
      },
      {
        device_id: "0192f7a1-0000-7000-8000-00000000d002",
        name: "Old tablet",
        platform: "Android 13",
        paired_at: now - 80 * day,
        last_seen_at: now - 2 * day,
        revoked_at: null,
        grants: {
          ...grants,
          admin: false,
          control_input: false,
          computer_use: false,
        },
        is_current: false,
      },
      {
        device_id: "0192f7a1-0000-7000-8000-00000000d003",
        name: "Retired phone",
        platform: "Android 12",
        paired_at: now - 100 * day,
        last_seen_at: now - 20 * day,
        revoked_at: now - 19 * day,
        grants,
        is_current: false,
      },
    ],
    identities: [
      {
        id: "sim-chat-telegram",
        channel: "telegram",
        external_user: "5550142",
        external_chat: "5550142",
        display_name: "Sam Rivers",
        linked_at: now - 4 * day,
        revoked_at: null,
        grants: { ...grants, admin: false, control_input: false },
        project_id: ids.pTide,
      },
      {
        id: "sim-chat-discord",
        channel: "discord",
        external_user: "1203344556677889900",
        external_chat: "1203344556677889911",
        display_name: "Alex Chen",
        linked_at: now - 2 * day,
        revoked_at: null,
        grants: { ...grants, admin: false, control_input: false },
        project_id: null,
      },
    ],
    // "123-456" is waiting for approval and "654-321" ran out before it was
    // approved; the computer holds no other codes.
    links: {
      "123-456": {
        channel: "telegram",
        external_user: "5550199",
        external_chat: "-1005550100",
        display_name: "Robin Park",
        project_id: ids.pTide,
        expires_at: now + 8 * 60_000,
      },
      "654-321": {
        channel: "telegram",
        external_user: "5550177",
        external_chat: "5550177",
        display_name: "Jo Lee",
        project_id: null,
        expires_at: now - 60_000,
      },
    },
    previews: [
      {
        preview_id: "sim-preview-down",
        project_id: ids.pTide,
        label: "Storybook",
        url: "http://studio.tail1234.ts.net:6006",
        reachable: false,
        checked_at: now - 60000,
      },
      {
        preview_id: "sim-preview-unknown",
        project_id: ids.pNotes,
        label: "Notes",
        url: "http://localhost:3000",
        reachable: null,
        checked_at: null,
      },
    ],
  };
}
export function settingsHarnesses(base: HarnessInfo[]): HarnessInfo[] {
  const example = base[0];
  if (!example) return base;
  return [
    ...base,
    {
      ...example,
      kind: "hermes",
      display_name: "Hermes",
      native_version: null,
      integration: "not_integrated",
      installed: {
        state: "unknown",
        reason: "Installation hasn’t been checked.",
      },
      account: {
        state: "signed_out",
        remedy: "Open Hermes on the computer and complete its native sign-in.",
      },
      models: [],
      efforts: [],
      capabilities: Object.fromEntries(
        Object.keys(example.capabilities).map((k) => [
          k,
          {
            state: "unsupported",
            reason: "This agent is not connected to the app.",
          },
        ]),
      ) as HarnessInfo["capabilities"],
    },
  ];
}
/** The pending link behind a code, as the computer checks it before approval. */
function pendingLink(host: SimHost, code: string): PendingChannelLink {
  const link = host.links[code];
  if (!link) throw { code: "not_found", message: "Record not found" };
  if (link.expires_at <= Date.now())
    throw { code: "expired", message: "Channel link expired" };
  return link;
}
export function settingsQuery(
  host: SimHost,
  q: Query & { kind: "channel_link" },
): QueryData {
  return { kind: "channel_link", link: pendingLink(host, q.link_code) };
}
export function settingsCommand(
  host: SimHost,
  c: Command,
): CommandResult | null {
  switch (c.command) {
    case "rename_device":
    case "revoke_device":
    case "set_device_grants":
    case "change_device_grants": {
      const device = host.devices.find((d) => d.device_id === c.device_id);
      if (!device)
        throw { code: "not_found", message: "That device is no longer listed." };
      if (device.revoked_at)
        throw (c.command === "set_device_grants" || c.command === "change_device_grants")
          ? {
              code: "conflict",
              message: "This device's access was revoked. Pair it again to give it access",
            }
          : {
              code: "already_resolved",
              message: "That device’s access is already revoked.",
            };
      if (c.command === "rename_device" && !c.name.trim())
        throw { code: "invalid_request", message: "A device name is required." };
      let grants = { ...device.grants };
      if (c.command === "set_device_grants") {
        if ((Object.keys(grants) as (keyof Grants)[]).some((key) => grants[key] !== c.expected[key]))
          throw { code: "device_grants_changed", message: "This device's permissions changed while you were editing. Review the current permissions and save again." };
        grants = { ...c.grants };
      } else if (c.command === "change_device_grants") {
        for (const key of Object.keys(grants) as (keyof Grants)[]) if (c.mask[key]) grants[key] = c.allowed;
        if (c.allowed) { grants.view ||= grants.control_input; grants.library_read ||= grants.library_write; }
        else { grants.control_input &&= grants.view; grants.library_write &&= grants.library_read; }
      }
      const others = host.devices.filter((d) => d.device_id !== device.device_id && !d.revoked_at);
      if (device.grants.admin && (c.command === "revoke_device" || !grants.admin) && others.length && !others.some((d) => d.grants.admin))
        throw { code: "last_manager", message: "Give another device Manage access first" };
      // As on the computer: a phone may take Accounts away but never give it.
      if (c.command !== "rename_device" && c.command !== "revoke_device" && grants.accounts && !device.grants.accounts)
        throw { code: "permission_denied", message: "Only the computer can give a device access to Accounts. Run “ophio devices allow <device> accounts” there." };
      host.devices = host.devices.map((d) =>
        d.device_id !== c.device_id
          ? d
          : c.command === "rename_device"
            ? { ...d, name: c.name }
            : c.command === "set_device_grants" || c.command === "change_device_grants"
              ? { ...d, grants }
              : { ...d, revoked_at: Date.now() },
      );
      host.emit({ type: "devices_changed", devices: host.devices });
      return null;
    }
    case "approve_channel_link": {
      const link = pendingLink(host, c.link_code);
      const sample = host.identities[0];
      if (!sample)
        throw {
          code: "unavailable",
          message: "No chat connector is configured.",
        };
      delete host.links[c.link_code];
      host.identities = [
        ...host.identities,
        {
          ...sample,
          id: `sim-chat-${c.link_code}`,
          channel: link.channel,
          display_name: link.display_name,
          external_user: link.external_user,
          external_chat: link.external_chat,
          linked_at: Date.now(),
          revoked_at: null,
          project_id: link.project_id,
        },
      ];
      host.emit({
        type: "channel_identities_changed",
        identities: host.identities,
      });
      return null;
    }
    case "refuse_channel_link": {
      pendingLink(host, c.link_code);
      delete host.links[c.link_code];
      return null;
    }
    case "revoke_channel_identity": {
      if (!host.identities.some((i) => i.id === c.identity_id))
        throw {
          code: "not_found",
          message: "That chat identity is no longer listed.",
        };
      host.identities = host.identities.map((i) =>
        i.id === c.identity_id ? { ...i, revoked_at: Date.now() } : i,
      );
      host.emit({
        type: "channel_identities_changed",
        identities: host.identities,
      });
      return null;
    }
    case "set_channel_project": {
      const identity = host.identities.find((i) => i.id === c.identity_id);
      if (!identity)
        throw {
          code: "not_found",
          message: "That chat identity is no longer listed.",
        };
      if (identity.revoked_at)
        throw {
          code: "conflict",
          message: "This chat's access was revoked. Link it again to choose a project",
        };
      if (c.project_id && !host.projects.some((p) => p.project_id === c.project_id))
        throw {
          code: "not_found",
          message: "That project is no longer on this computer",
        };
      // The project belongs to the chat, so everyone linked in it shares it.
      host.identities = host.identities.map((i) =>
        i.channel === identity.channel && i.external_chat === identity.external_chat
          ? { ...i, project_id: c.project_id }
          : i,
      );
      host.emit({
        type: "channel_identities_changed",
        identities: host.identities,
      });
      return null;
    }
    case "register_preview": {
      const url = new URL(c.url);
      if (
        !["http:", "https:"].includes(url.protocol) ||
        url.username ||
        url.password
      )
        throw {
          code: "invalid_request",
          message: "Use an HTTP or HTTPS URL without credentials.",
        };
      const preview: PreviewEndpoint = {
        preview_id: uuidv7(Date.now()),
        project_id: c.project_id,
        label: c.label,
        url: c.url,
        reachable: null,
        checked_at: null,
      };
      host.previews = [...host.previews, preview];
      host.emit({
        type: "previews_changed",
        project_id: c.project_id,
        previews: host.previews.filter((p) => p.project_id === c.project_id),
      });
      return { kind: "preview_registered", preview_id: preview.preview_id };
    }
    case "check_preview":
    case "remove_preview": {
      const preview = host.previews.find((p) => p.preview_id === c.preview_id);
      if (!preview)
        throw { code: "not_found", message: "That preview was removed." };
      host.previews =
        c.command === "remove_preview"
          ? host.previews.filter((p) => p.preview_id !== c.preview_id)
          : host.previews.map((p) =>
              p.preview_id === c.preview_id
                ? {
                    ...p,
                    checked_at: Date.now(),
                    // A shown page's server answers once it was started again.
                    reachable: p.start
                      ? (c.command === "check_preview" && !!c.start) || !!p.reachable
                      : !p.url.includes(":6006"),
                  }
                : p,
            );
      host.emit({
        type: "previews_changed",
        project_id: preview.project_id,
        previews: host.previews.filter(
          (p) => p.project_id === preview.project_id,
        ),
      });
      return null;
    }
    default:
      throw new Error("Not a settings command");
  }
}
