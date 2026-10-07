import "@testing-library/jest-dom/vitest";

import { cleanup, configure } from "@testing-library/react";

// Pages are lazy routes: the first render of one in a test file imports and
// transforms its module, which can take longer than findBy's default second
// on a loaded machine (seen with /results in app and settings tests).
configure({ asyncUtilTimeout: 4000 });

// jsdom lacks a few browser APIs that Radix primitives use.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
if (!("ResizeObserver" in globalThis)) {
  Object.defineProperty(globalThis, "ResizeObserver", { value: ResizeObserverStub });
}
Element.prototype.scrollIntoView = () => {};
Element.prototype.hasPointerCapture = () => false;
Element.prototype.releasePointerCapture = () => {};

afterEach(() => {
  cleanup();
  localStorage.clear();
  document.documentElement.classList.remove("dark");
});
