import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";
const source = await readFile(
  new URL("../src/common/libraryFilter.ts", import.meta.url),
  "utf8",
);
const { outputText } = ts.transpileModule(source, {
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ES2022,
  },
});
const {
  emptyLibraryFilter,
  activeLibraryFilterCount,
  toggleFacet,
  parseKeywordInput,
  keywordLeaf,
  keywordDepth,
} = await import(
  "data:text/javascript;base64," + Buffer.from(outputText).toString("base64")
);
test("empty filters are independent and zero-valued attributes count as active", () => {
  const a = emptyLibraryFilter(),
    b = emptyLibraryFilter();
  a.keywordIds.push(7);
  assert.deepEqual(b.keywordIds, []);
  assert.equal(activeLibraryFilterCount(b), 0);
  b.flag = 0;
  b.ratingMin = 0;
  b.text = " ";
  assert.equal(activeLibraryFilterCount(b), 2);
  b.cameras = ["Canon", "Nikon"];
  assert.equal(activeLibraryFilterCount(b), 3);
});
test("facet click replaces selection, modifiers add or remove, repeated plain click clears", () => {
  assert.deepEqual(toggleFacet(["a"], "b", false), ["b"]);
  assert.deepEqual(toggleFacet(["a"], "a", false), []);
  assert.deepEqual(toggleFacet(["a"], "b", true), ["a", "b"]);
  assert.deepEqual(toggleFacet(["a", "b"], "a", true), ["b"]);
});
test("keyword input preserves multiword keywords and canonicalizes nested paths", () => {
  assert.deepEqual(
    parseKeywordInput(
      " Imóveis> Sala de estar, cozinha; Imóveis > Sala de estar\nÁrea de serviço",
    ),
    ["Imóveis > Sala de estar", "cozinha", "Área de serviço"],
  );
  assert.equal(keywordLeaf("Imóveis > Imóvel 1 > Sala"), "Sala");
  assert.equal(keywordDepth("Imóveis > Imóvel 1 > Sala"), 2);
  assert.deepEqual(parseKeywordInput("  , ;\n"), []);
});
