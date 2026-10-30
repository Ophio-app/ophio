// SPDX-License-Identifier: Apache-2.0
import type { SimHost } from "./host";
import { ids } from "./scenario";
import type {
  WindowAction,
  WindowCapabilities,
  WindowCatalog,
  WindowControl,
  WindowActionReceipt,
} from "../../protocol/types";

export class WindowsSim {
  backend =
    new URLSearchParams(globalThis.location?.search ?? "").get(
      "windowBackend",
    ) ?? "hyprland";
  empty = false;
  error = false;
  focusRefused = false;
  readonly actions: WindowActionReceipt[] = [];
  #windows = [
    {
      window_id: "editor",
      title: "retry.rs — garden",
      app: "Code",
      workspace: "2",
      monitor: "HDMI-A-1",
      focused: true,
    },
    {
      window_id: "browser",
      title: "Garden · local preview",
      app: "Firefox",
      workspace: "2",
      monitor: "HDMI-A-1",
      focused: false,
    },
  ];
  constructor(private host: SimHost) {}
  capabilities(): WindowCapabilities {
    return {
      backend: this.backend,
      windows:
        this.backend === "gnome"
          ? {
              state: "unsupported",
              reason:
                "GNOME does not expose other apps’ windows without an extension. You can still open an installed app.",
            }
          : { state: "available" },
      open_apps: { state: "available" },
    };
  }
  catalog(search: string): WindowCatalog {
    if (this.error)
      throw {
        code: "native_failure",
        message:
          "The desktop could not list its windows. Try again after checking the computer.",
      };
    const needle = search.toLocaleLowerCase();
    return {
      capabilities: this.capabilities(),
      windows:
        this.empty || this.backend === "gnome"
          ? []
          : this.#windows
              .filter((w) =>
                `${w.title} ${w.app}`.toLocaleLowerCase().includes(needle),
              )
              .map((w) => ({ ...w })),
      apps: this.empty
        ? []
        : [
            { app_id: "firefox.desktop", name: "Firefox" },
            { app_id: "code.desktop", name: "Visual Studio Code" },
            { app_id: "files.desktop", name: "Files" },
          ].filter((a) => a.name.toLocaleLowerCase().includes(needle)),
      observed_at: Date.now(),
      truncated: false,
    };
  }
  act(control: WindowControl, action: WindowAction): WindowActionReceipt {
    const view = this.host.control.find(
        (v) => v.target_id === control.target_id,
      ),
      lease = view?.lease;
    const geometry = this.host.targets.find(
      (t) => t.target_id === control.target_id,
    )?.geometry.revision;
    if (
      !lease ||
      lease.owner.kind !== "human" ||
      lease.owner.device_id !== ids.device ||
      lease.state !== "active" ||
      view?.input_state !== "human" ||
      lease.lease_id !== control.lease_id ||
      lease.generation !== control.generation ||
      lease.boot_epoch !== control.boot_epoch ||
      geometry !== control.geometry_revision
    )
      throw {
        code: "permission_denied",
        message: "Enable control again after reviewing the current picture.",
      };
    if (
      action.action !== "open_app" &&
      !this.#windows.some((w) => w.window_id === action.window_id)
    )
      throw {
        code: "not_found",
        message: "This window is no longer available.",
      };
    if (action.action === "focus" && this.focusRefused)
      throw {
        code: "permission_denied",
        message:
          "Windows refused to move focus. Finish the active menu or interaction on the computer, then try again.",
      };
    if (action.action === "focus")
      this.#windows = this.#windows.map((w) => ({
        ...w,
        focused: w.window_id === action.window_id,
      }));
    // Closing is a request: an app may still be asking about unsaved work.
    const receipt: WindowActionReceipt = {
      target_id: control.target_id,
      lease_id: control.lease_id,
      requested_at: Date.now(),
      action,
      outcome:
        action.action === "focus"
          ? "focused"
          : action.action === "close"
            ? "close_requested"
            : "app_launch_requested",
      detail:
        action.action === "focus"
          ? "The window has focus."
          : action.action === "close"
            ? "Close requested. Check the desktop for an unsaved-work prompt."
            : "App launch requested. Check the desktop for its window.",
    };
    this.actions.push(receipt);
    return receipt;
  }
}
