// SPDX-License-Identifier: Apache-2.0
import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import { useCrampedLayout, useSidewaysLayout, useTightLayout } from "./keyboard";

it("calls the screen cramped from its visible height, keyboard included, without making it tight", () => {
  const root = document.documentElement;
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 780 });
  renderHook(() => useTightLayout());
  expect(root.classList.contains("app-cramped")).toBe(false);
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 250 });
  window.dispatchEvent(new Event("resize"));
  expect(root.classList.contains("app-cramped")).toBe(true);
  expect(root.classList.contains("app-tight")).toBe(false);
});

it("tells a component when the visible height is cramped and when it recovers", () => {
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 780 });
  window.dispatchEvent(new Event("resize"));
  const { result } = renderHook(() => useCrampedLayout());
  expect(result.current).toBe(false);
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 250 });
  act(() => { window.dispatchEvent(new Event("resize")); });
  expect(result.current).toBe(true);
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 780 });
  act(() => { window.dispatchEvent(new Event("resize")); });
  expect(result.current).toBe(false);
});

it("calls a short screen from 600px wide a phone on its side, including one beside Android's buttons", () => {
  const size = (width: number, height: number) => {
    Object.defineProperties(window, { innerWidth: { value: width, configurable: true }, innerHeight: { value: height, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  };
  const { result } = renderHook(() => useSidewaysLayout());
  try {
    size(640, 320);
    expect(result.current).toBe(true);
    expect(document.documentElement.classList.contains("app-sideways")).toBe(true);
    size(390, 844);
    expect(result.current).toBe(false);
    expect(document.documentElement.classList.contains("app-sideways")).toBe(false);
    // Too narrow to share a row, even when short.
    size(560, 300);
    expect(result.current).toBe(false);
  } finally {
    size(1024, 768);
  }
});
