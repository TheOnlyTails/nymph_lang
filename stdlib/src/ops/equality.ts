function equals<T>(self: T, other: T): boolean {
  // ints, floats, strings, chars, booleans
  if (self === other) return true;

  if (
    Array.isArray(self) &&
    Array.isArray(other) &&
    self.length === other.length &&
    self.every((val, i) => equals(val, other[i]))
  )
    return true;

  if (
    self instanceof Map &&
    other instanceof Map &&
    self.size === other.size &&
    self.entries().every(([k, v]) => equals(other.get(k), v))
  )
    return true;

  if (
    typeof self === "object" &&
    typeof other === "object" &&
    self &&
    other &&
    "~tag" in self &&
    "~tag" in other &&
    typeof self["~tag"] === "string" &&
    typeof other["~tag"] === "string" &&
    self["~tag"] === other["~tag"] &&
    Object.keys(self).length === Object.keys(other).length &&
    Object.entries(self).every(([k, v]) => equals(v, other[k]))
  ) {
    return true;
  }

  return false;
}

export { equals as Equals_Other_self_T$equals };
