export interface LibraryFilter {
  text: string;
  field: string;
  mode: string;
  ratingMin: number | null;
  flag: number | null;
  favorite: boolean;
  years: string[];
  cameras: string[];
  lenses: string[];
  labels: string[];
  keywordIds: number[];
  withoutKeywords: boolean;
  fileTypes: string[];
}
export function emptyLibraryFilter(): LibraryFilter {
  return {
    text: "",
    field: "all",
    mode: "all",
    ratingMin: null,
    flag: null,
    favorite: false,
    years: [],
    cameras: [],
    lenses: [],
    labels: [],
    keywordIds: [],
    withoutKeywords: false,
    fileTypes: [],
  };
}
export function activeLibraryFilterCount(filter: LibraryFilter) {
  return (
    Number(!!filter.text.trim()) +
    Number(filter.ratingMin !== null) +
    Number(filter.flag !== null) +
    Number(filter.favorite) +
    Number(filter.withoutKeywords) +
    ["years", "cameras", "lenses", "labels", "keywordIds", "fileTypes"].reduce(
      (count, key) => count + Number(filter[key].length > 0),
      0,
    )
  );
}
export function toggleFacet<T>(values: T[], value: T, additive: boolean): T[] {
  if (!additive)
    return values.length === 1 && values[0] === value ? [] : [value];
  return values.includes(value)
    ? values.filter((item) => item !== value)
    : [...values, value];
}
export function keywordLeaf(name: string) {
  return name.split(" > ").at(-1) || name;
}
export function keywordDepth(name: string) {
  return name.split(" > ").length - 1;
}
export function parseKeywordInput(value: string): string[] {
  return [
    ...new Set(
      value
        .split(/[,;\n]/)
        .map((name) =>
          name
            .split(">")
            .map((part) => part.trim())
            .join(" > "),
        )
        .filter(Boolean),
    ),
  ];
}
