// SPDX-License-Identifier: Apache-2.0
import schema from "./stateSchema.json";
export const shapeKeys = new Set<string>(schema.keys);
export const enumValues: Record<string, readonly string[]> = schema.values;
