// SPDX-License-Identifier: Apache-2.0
import type {
  Checklist, ChecklistFile, ChecklistItem, ChecklistSummary, Command, CommandResult, FileDeliverySettings, FileDeliveryView, ProtocolError,
} from "../../protocol/types";
import { sha256, type SimHost } from "./host";
import { ids } from "./scenario";

const fail = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message } as ProtocolError);

const ENV_FILE = "# tidewater-api: the webhook secret Stripe shows once, for the server.\n# Put the whsec_ value Stripe gives you after the equals sign.\nSTRIPE_WEBHOOK_SECRET=\n";
const LOGO = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="#0e7490"/><path d="M10 38c6-6 12-6 18 0s12 6 18 0 8-5 8-5v9c-6 6-12 6-18 0s-12-6-18 0-8 5-8 5z" fill="#ecfeff"/><circle cx="44" cy="20" r="6" fill="#fde68a"/></svg>\n';

/** The computer's checklists (`crates/services/src/checklist.rs`), as the phone
 * sees them: tidewater-api has one an agent wrote, waiting on Stripe access. */
export class ChecklistSim {
  lists = new Map<string, Checklist>();
  delivery: FileDeliverySettings = { delivery: { kind: "phone" }, revision: 0 };
  /** What the computer could deliver to. */
  taildrop: string[] | undefined = ["sam-laptop", "pixel-tablet"];
  drives: string[] | undefined = ["gdrive", "dropbox"];
  /** Items finished that an agent waited on, in order: the agent started again. */
  resumed: { itemId: string; waiting: string }[] = [];
  #bytes = new Map<string, Uint8Array>();
  #notes = 0;
  #followups = new Map<string, ReturnType<typeof setTimeout>>();
  readonly ready: Promise<void>;

  dispose(): void {
    for (const timer of this.#followups.values()) clearTimeout(timer);
    this.#followups.clear();
  }
  constructor(private readonly host: SimHost) {
    this.ready = this.#seed();
  }

  async #file(name: string, media_type: string, text: string): Promise<ChecklistFile> {
    const bytes = new TextEncoder().encode(text);
    const hash = await sha256(bytes);
    this.#bytes.set(hash, bytes);
    return { name, size: bytes.length, sha256: hash, media_type };
  }

  async #seed(): Promise<void> {
    const now = Date.now();
    const at = (min: number) => now - min * 60_000;
    const env = await this.#file("tidewater.env", "text/plain", ENV_FILE);
    const logo = await this.#file("tidewater.svg", "image/svg+xml", LOGO);
    const item = (i: Partial<ChecklistItem> & Pick<ChecklistItem, "item_id" | "title" | "steps">): ChecklistItem =>
      ({ priority: "medium", added_at: at(90), changed_at: at(90), task_id: ids.tHeaders, ...i });
    this.lists.set(ids.pTide, {
      project_id: ids.pTide,
      title: "Launch tidewater-api",
      summary: "What only you can do before payments go live: accounts, access and one choice.",
      theme: { name: "Tidewater", accent: "#0e7490", font: "sans", logo },
      task_id: ids.tHeaders,
      created_at: at(90),
      updated_at: at(12),
      revision: 3,
      notes: [
        { note_id: "n1", from: "person", text: "Do I need a business account for Stripe, or is my personal one fine?", at: at(40) },
        { note_id: "n2", from: "agent", text: "Your personal sign-in is fine. Stripe asks for business details when you turn on live payments, in step 3 of “Turn on live payments”.", at: at(38) },
      ],
      sections: [
        {
          section_id: "accounts",
          title: "Accounts and access",
          items: [
            item({
              item_id: "stripe-key",
              title: "Give the agent a Stripe test key",
              why: "The agent needs it to set up the payment webhook and test a checkout.",
              minutes: 5,
              priority: "blocker",
              waiting: "Sets up the Stripe webhook and runs a test payment",
              steps: [
                { step_id: "open", text: "Sign in to Stripe and open **Developers → API keys**, in test mode.", url: "https://dashboard.stripe.com/test/apikeys", done_at: at(20) },
                { step_id: "create", text: "Choose **Create restricted key**, name it as below, and allow **Write** for Webhook endpoints and Checkout Sessions.", copy: "tidewater-agent" },
                { step_id: "save", text: "Copy the key, then on the computer run this and paste it when it asks. The key never goes in chat.", copy: "ophio accounts set-key stripe" },
              ],
            }),
            item({
              item_id: "webhook-test",
              title: "Test the Stripe webhook after access is ready",
              owner: "agent",
              why: "The agent will run a test checkout after you give it the test key.",
              after: ["stripe-key"],
              steps: [{ step_id: "checkout", text: "Run a test checkout and verify its webhook." }],
            }),
            item({
              item_id: "webhook-secret",
              title: "Put the webhook secret on the server",
              why: "Without it the server turns away Stripe’s payment messages.",
              minutes: 4,
              priority: "high",
              after: ["stripe-key"],
              steps: [
                { step_id: "file", text: "Get the file below. It has one line for the secret.", file: env },
                { step_id: "fill", text: "In Stripe, open **Developers → Webhooks → tidewater**, choose **Reveal** under Signing secret, and paste it after `STRIPE_WEBHOOK_SECRET=`.", url: "https://dashboard.stripe.com/test/webhooks" },
                { step_id: "place", text: "Save the file as `/etc/tidewater/tidewater.env` on the server." },
              ],
            }),
            item({
              item_id: "dns",
              title: "Point api.tidewater.dev at the server",
              minutes: 10,
              priority: "low",
              done_at: at(60),
              steps: [
                { step_id: "record", text: "At your domain registrar, add an **A** record for `api` with this address.", copy: "203.0.113.24", done_at: at(60) },
              ],
            }),
          ],
        },
        {
          section_id: "choices",
          title: "Choices",
          items: [
            item({
              item_id: "currency",
              title: "Choose the currency for prices",
              why: "Prices and receipts are shown in it.",
              minutes: 1,
              steps: [],
              ask: {
                question: "Which currency do customers pay in?",
                options: [
                  { label: "US dollars", detail: "Stripe pays out in dollars" },
                  { label: "Euros" },
                  { label: "Both, by the customer’s country" },
                ],
              },
              notes: [{ note_id: "n3", from: "person", text: "Can I change this later?", at: at(12) }],
            }),
            item({
              item_id: "live",
              title: "Turn on live payments",
              priority: "high",
              due: "2026-11-02",
              minutes: 20,
              after: ["webhook-secret", "currency"],
              steps: [
                { step_id: "activate", text: "In Stripe, choose **Activate payments**.", url: "https://dashboard.stripe.com/account/onboarding" },
                { step_id: "bank", text: "Add the bank account payouts go to." },
                { step_id: "business", text: "Give your business details. As a sole trader, choose **Individual**." },
              ],
            }),
          ],
        },
      ],
    });
    this.lists.set(ids.pNotes, {
      project_id: ids.pNotes,
      title: "Organize field notes",
      summary: "One review from you, then the agent can finish tagging.",
      task_id: ids.tNotes,
      created_at: at(55), updated_at: at(7), revision: 1,
      sections: [{ section_id: "review", title: "Review", items: [
        item({ item_id: "review-tags", title: "Review the proposed topic tags", task_id: ids.tNotes,
          steps: [{ step_id: "approve", text: "Check the proposed tags and tick when they look right." }] }),
        item({ item_id: "apply-tags", title: "Apply the approved tags", owner: "agent", task_id: ids.tNotes,
          after: ["review-tags"], steps: [{ step_id: "apply", text: "Tag the notes using the approved topics." }] }),
      ] }],
    });
  }

  async query(q: { kind: "checklists" } | { kind: "checklist"; project_id: string } | { kind: "file_delivery" }) {
    await this.ready;
    switch (q.kind) {
      case "checklists": {
        const checklists = [...this.lists.values()].map(summary).sort((a, b) => b.updated_at - a.updated_at);
        return { kind: "checklists" as const, checklists };
      }
      case "checklist": return { kind: "checklist" as const, checklist: structuredClone(this.lists.get(q.project_id) ?? null) };
      case "file_delivery": return { kind: "file_delivery" as const, view: this.view() };
    }
  }

  view(): FileDeliveryView {
    return {
      settings: structuredClone(this.delivery),
      taildrop: this.taildrop && [...this.taildrop],
      folders: [
        { label: "Downloads", path: "/home/sam/Downloads" },
        { label: "Google Drive", path: "/home/sam/Google Drive" },
      ],
      drives: this.drives && [...this.drives],
    };
  }

  async act(c: Command, expectedRevision: number | null): Promise<CommandResult | null> {
    await this.ready;
    const now = Date.now();
    switch (c.command) {
      case "checklist_tick": {
        const list = this.#list(c.project_id);
        const item = this.#item(list, c.item_id);
        if (item.owner === "agent") throw fail("invalid_request", `“${item.title}” is the agent's to do. Send it feedback instead.`);
        const was = item.done_at !== undefined;
        if (c.step_id !== undefined) {
          const step = item.steps.find((s) => s.step_id === c.step_id);
          if (!step) throw fail("not_found", "That step is no longer on the item");
          step.done_at = c.done ? step.done_at ?? now : undefined;
          item.done_at = c.done ? (finished(item) ? item.done_at ?? now : item.done_at) : undefined;
        } else if (c.done) {
          if (item.ask && !item.ask.answer) throw fail("invalid_request", "Answer the item's question before ticking it off");
          for (const s of item.steps) s.done_at ??= now;
          item.done_at ??= now;
        } else {
          item.done_at = undefined;
        }
        item.changed_at = now;
        if (!was && item.done_at !== undefined && item.waiting) this.resumed.push({ itemId: item.item_id, waiting: item.waiting });
        return this.#save(list, now);
      }
      case "checklist_answer": {
        const text = words(c.answer, "the answer");
        const list = this.#list(c.project_id);
        const item = this.#item(list, c.item_id);
        if (item.owner === "agent") throw fail("invalid_request", "An agent item has no question to answer; send it feedback instead");
        if (!item.ask) throw fail("invalid_request", "That item asks nothing");
        item.ask.answer = { text, at: now };
        if (item.done_at === undefined && finished(item)) {
          item.done_at = now;
          if (item.waiting) this.resumed.push({ itemId: item.item_id, waiting: item.waiting });
        }
        item.changed_at = now;
        return this.#save(list, now);
      }
      case "checklist_ask": {
        const text = words(c.text, "the question");
        const list = this.#list(c.project_id);
        const note = { note_id: `sim-${++this.#notes}`, from: "person" as const, text, at: now };
        if (c.item_id !== undefined) {
          const item = this.#item(list, c.item_id);
          item.notes = [...(item.notes ?? []), note];
        } else {
          list.notes = [...(list.notes ?? []), note];
        }
        return this.#save(list, now);
      }
      case "checklist_deliver": {
        const list = this.#list(c.project_id);
        const file = files(list).find((f) => f.sha256 === c.sha256);
        if (!file) throw fail("not_found", "That file is no longer on the checklist");
        const d = this.delivery.delivery;
        const project = this.host.projects.find((p) => p.project_id === c.project_id)?.name ?? "project";
        switch (d.kind) {
          case "phone": throw fail("invalid_request", "Checklist files come to the phone; download it there");
          case "folder": return { kind: "delivered", to: `${d.path}/${project}/${file.name}` };
          case "taildrop": return { kind: "delivered", to: `${d.device}, by Taildrop` };
          case "drive": return { kind: "delivered", to: `${d.remote}:${d.folder ? `${d.folder}/` : ""}${project}/${file.name}` };
        }
        return null;
      }
      case "remove_checklist": {
        this.#list(c.project_id);
        this.lists.delete(c.project_id);
        this.#changed(c.project_id);
        return null;
      }
      case "set_file_delivery": {
        if (expectedRevision !== null && expectedRevision !== this.delivery.revision) throw fail("stale_revision", "Where files go changed. Review it again.");
        const d = c.delivery;
        if (d.kind === "folder" && !d.path.startsWith("/")) throw fail("invalid_request", "Give a folder that is on this computer");
        if (d.kind === "taildrop" && !d.device.trim()) throw fail("invalid_request", "Give the device's name as Taildrop lists it");
        if (d.kind === "drive" && d.folder.split("/").some((p) => p === ".." || p === ".")) throw fail("invalid_request", "Give the folder in the drive as a plain path, such as Ophio");
        this.delivery = { delivery: structuredClone(d), revision: this.delivery.revision + 1 };
        return null;
      }
      default: return null;
    }
  }

  /** A file kept with `projectId`'s checklist, for a download. */
  async file(projectId: string, hash: string): Promise<{ name: string; bytes: Uint8Array }> {
    await this.ready;
    const list = this.lists.get(projectId);
    const file = list && files(list).find((f) => f.sha256 === hash);
    const bytes = this.#bytes.get(hash);
    if (!file || !bytes) throw fail("not_found", "That file is no longer on the checklist");
    return { name: file.name, bytes };
  }

  /** An agent answers the person's latest question, as `checklist_reply` would. */
  reply(projectId: string, itemId: string | null, text: string): void {
    const list = this.#list(projectId);
    const note = { note_id: `sim-${++this.#notes}`, from: "agent" as const, text, at: Date.now() };
    if (itemId === null) list.notes = [...(list.notes ?? []), note];
    else { const item = this.#item(list, itemId); item.notes = [...(item.notes ?? []), note]; }
    this.#save(list, Date.now());
  }

  #list(projectId: string): Checklist {
    const list = this.lists.get(projectId);
    if (!list) throw fail("not_found", "This project has no checklist");
    return list;
  }

  #item(list: Checklist, id: string): ChecklistItem {
    const item = list.sections.flatMap((s) => s.items).find((i) => i.item_id === id);
    if (!item) throw fail("not_found", "That item is no longer on the checklist");
    return item;
  }

  #save(list: Checklist, now: number): null {
    list.updated_at = now;
    list.revision += 1;
    this.#syncFollowups(list);
    this.#changed(list.project_id);
    return null;
  }

  #syncFollowups(list: Checklist): void {
    const items = list.sections.flatMap((section) => section.items);
    const done = new Set(items.filter((item) => item.done_at !== undefined).map((item) => item.item_id));
    for (const item of items.filter((candidate) => candidate.owner === "agent" && candidate.after?.length)) {
      const key = `${list.project_id}:${item.item_id}`;
      const ready = item.done_at === undefined && item.after!.every((id) => done.has(id));
      if (!ready) {
        const timer = this.#followups.get(key);
        if (timer) clearTimeout(timer);
        this.#followups.delete(key);
      } else if (!this.#followups.has(key)) {
        this.#followups.set(key, setTimeout(() => {
          this.#followups.delete(key);
          const current = this.lists.get(list.project_id);
          if (!current) return;
          const currentItems = current.sections.flatMap((section) => section.items);
          const target = currentItems.find((candidate) => candidate.item_id === item.item_id);
          if (!target || target.done_at !== undefined || !target.after?.every((id) => currentItems.some((candidate) => candidate.item_id === id && candidate.done_at !== undefined))) return;
          const now = Date.now();
          target.done_at = now;
          target.changed_at = now;
          for (const step of target.steps) step.done_at ??= now;
          this.#save(current, now);
        }, 800));
      }
    }
  }

  #changed(projectId: string): void {
    this.host.emit({ type: "checklist_changed", project_id: projectId }, { kind: "companion" }, "observed");
  }
}

/** Whether every step is ticked and the question, if any, answered. */
function finished(item: ChecklistItem): boolean {
  return item.steps.every((s) => s.done_at !== undefined) && (!item.ask || !!item.ask.answer);
}

function words(text: string, what: string): string {
  const t = text.trim();
  if (!t) throw fail("invalid_request", `Write ${what} first`);
  if (t.length > 4000) throw fail("invalid_request", `Make ${what} shorter`);
  return t;
}

function files(list: Checklist): ChecklistFile[] {
  const steps = list.sections.flatMap((s) => s.items).flatMap((i) => i.steps).flatMap((s) => (s.file ? [s.file] : []));
  return list.theme?.logo ? [...steps, list.theme.logo] : steps;
}

function summary(list: Checklist): ChecklistSummary {
  let open = 0, done = 0, forAgent = 0, waiting = 0, blockers = 0, unanswered = 0;
  const last = (notes: Checklist["notes"]) => Number(notes?.at(-1)?.from === "person");
  const items = list.sections.flatMap((s) => s.items);
  for (const item of items) {
    if (item.owner === "agent") forAgent += Number(item.done_at === undefined);
    else if (item.done_at !== undefined) done += 1;
    else { open += 1; waiting += Number(!!item.waiting); blockers += Number(item.priority === "blocker"); }
    unanswered += last(item.notes);
  }
  unanswered += last(list.notes);
  const isDone = new Set(items.filter((i) => i.done_at !== undefined).map((i) => i.item_id));
  const here = new Set(items.map((i) => i.item_id));
  const openItems = items.filter((i) => i.owner !== "agent" && i.done_at === undefined);
  const next = openItems.find((i) => (i.after ?? []).every((a) => isDone.has(a) || !here.has(a)));
  return {
    project_id: list.project_id, title: list.title, open, done, for_agent: forAgent, waiting, blockers, unanswered,
    next: next?.title, accent: list.theme?.accent, updated_at: list.updated_at, revision: list.revision,
  };
}
