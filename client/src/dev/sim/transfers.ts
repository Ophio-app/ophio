// SPDX-License-Identifier: Apache-2.0
export interface UploadStage {
  id: string;
  parts: Uint8Array[];
  received: number;
}
export class UploadStaging {
  private stages = new Map<string, UploadStage>();
  delay = 100;
  open(id: string): UploadStage {
    let stage = this.stages.get(id);
    if (!stage) {
      stage = { id, parts: [], received: 0 };
      this.stages.set(id, stage);
    }
    return stage;
  }
  remove(id: string) {
    this.stages.delete(id);
  }
}
export function numberedPath(path: string, occupied: Set<string>): string {
  if (!occupied.has(path)) return path;
  const slash = path.lastIndexOf("/"),
    dot = path.lastIndexOf("."),
    split = dot > slash + 1 ? dot : path.length;
  for (let n = 1; ; n++) {
    const next = `${path.slice(0, split)} (${n})${path.slice(split)}`;
    if (!occupied.has(next)) return next;
  }
}
