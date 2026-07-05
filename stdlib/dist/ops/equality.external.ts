export function equals<T>($_this: T, other: T): boolean {
	// ints, floats, strings, chars, booleans
	if ($_this === other) return true;

	if (
		Array.isArray($_this) &&
		Array.isArray(other) &&
		$_this.length === other.length &&
		$_this.every((val, i) => equals(val, other[i]))
	)
		return true;

	if (
		$_this instanceof Map &&
		other instanceof Map &&
		$_this.size === other.size &&
		$_this.entries().every(([k, v]) => equals(other.get(k), v))
	)
		return true;

	if (
		typeof $_this === "object" &&
		typeof other === "object" &&
		$_this &&
		other &&
		"~tag" in $_this &&
		"~tag" in other &&
		typeof $_this["~tag"] === "string" &&
		typeof other["~tag"] === "string" &&
		$_this["~tag"] === other["~tag"] &&
		Object.keys($_this).length === Object.keys(other).length &&
		Object.entries($_this).every(([k, v]) => equals(v, other[k]))
	) {
		return true;
	}

	return false;
}
