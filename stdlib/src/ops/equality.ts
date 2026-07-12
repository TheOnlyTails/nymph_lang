const TAG = Symbol.for("nymph.tag");

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

	// Enum variants carry their identity under the shared `TAG` symbol. Compare that
	// identity, then the string-keyed fields — symbol keys are invisible to
	// `Object.keys`/`Object.entries`, so `TAG` is excluded from the field walk.
	if (
		typeof $_this === "object" &&
		typeof other === "object" &&
		$_this &&
		other &&
		TAG in $_this &&
		TAG in other &&
		$_this[TAG] === other[TAG] &&
		Object.keys($_this).length === Object.keys(other).length &&
		Object.entries($_this).every(([k, v]) => equals(v, other[k]))
	) {
		return true;
	}

	return false;
}
