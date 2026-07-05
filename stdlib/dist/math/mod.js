export const pi = 3.141592653589793;
export const tau = 6.283185307179586;
export const e = 2.718281828459045;
export const phi = 1.618033988749895;
export function abs(x) {
	return x.greater_than(0) ? x : x.negate();
}
export function abs(x) {
	return x.greater_than(0) ? x : x.negate();
}
import { sin } from './mod.external.ts';
export { sin };
import { cos } from './mod.external.ts';
export { cos };
import { tan } from './mod.external.ts';
export { tan };
import { asin } from './mod.external.ts';
export { asin };
import { acos } from './mod.external.ts';
export { acos };
import { atan } from './mod.external.ts';
export { atan };
import { sinh } from './mod.external.ts';
export { sinh };
import { cosh } from './mod.external.ts';
export { cosh };
import { tanh } from './mod.external.ts';
export { tanh };
import { asinh } from './mod.external.ts';
export { asinh };
import { acosh } from './mod.external.ts';
export { acosh };
import { atanh } from './mod.external.ts';
export { atanh };
import { atan2 } from './mod.external.ts';
export { atan2 };
export function sign(x) {
	return (() => {
		const __match$0 = x;
		if (__match$0 === 0) {
			return 0;
		} else if (true) {
			return 1;
		} else if (true) {
			return 1 .negate();
		}
		return undefined;
	})();
}
export function sign(x) {
	return (() => {
		const __match$1 = x;
		if (__match$1 === 0) {
			return 0;
		} else if (true) {
			return 1;
		} else if (true) {
			return 1 .negate();
		}
		return undefined;
	})();
}
import { floor } from './mod.external.ts';
export { floor };
import { ceil } from './mod.external.ts';
export { ceil };
import { round } from './mod.external.ts';
export { round };
export function midpoint(x, y) {
	return x.plus(y).divide(2);
}
export function midpoint(x, y) {
	return x.plus(y).divide(2);
}
export function sqrt(x) {
	return x.power(.5);
}
export const max_int = 2 .power(63).minus(1);
export const min_int = 2 .negate().power(63);
import { max_float } from './mod.external.ts';
export { max_float };
import { min_float } from './mod.external.ts';
export { min_float };
