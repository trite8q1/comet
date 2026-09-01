import { describe, expect, it } from "vitest";
import { splitName } from "./workos";

describe("splitName", () => {
  it("puts the first word in firstName and the rest in lastName", () => {
    expect(splitName("Mary Ann Smith")).toEqual({ firstName: "Mary", lastName: "Ann Smith" });
  });

  it("leaves lastName empty for a single word", () => {
    expect(splitName("Cher")).toEqual({ firstName: "Cher", lastName: "" });
  });

  it("ignores surrounding and repeated whitespace", () => {
    expect(splitName("  Ada   Lovelace \n")).toEqual({ firstName: "Ada", lastName: "Lovelace" });
  });
});
