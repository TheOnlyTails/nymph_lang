import * as result from './result.js';
import { Result } from './result.js';
import * as default from './default.js';
import { Default } from './default.js';
export const Option = {};
Option.Some = (value) => ;
Option.None = Object.freeze({ '~tag': 'None' });
Option.is_some = () => ;
Option.is_some_and = (f) => ;
Option.is_none = () => ;
Option.is_none_or = (f) => ;
Option.map = (f) => ;
Option.map_or = (default, f) => ;
Option.map_or_else = (default, f) => ;
Option.map_or_default = (f) => ;
Option.unwrap_or = (default) => ;
Option.unwrap_or_else = (default) => ;
Option.inspect = (f) => ;
Option.ok_or = (error) => ;
Option.ok_or_else = (error) => ;
Option.and = (other) => ;
Option.and_then = (other) => ;
Option.filter = (predicate) => ;
Option.or = (other) => ;
Option.or_else = (f) => ;
Option.xor = (other) => ;
Option.zip = (other) => ;
Option.prototype.unwrap_or_default = function() {
	return this.unwrap_or(T.default());
};
Option.prototype.unzip = function() {
	return (() => {
		const __match$16 = this;
		if (__match$16['~tag'] === 'Some') {
			const _ = __match$16.value;
			return [Some(a), Some(b)];
		} else if (true) {
			const None = __match$16;
			return [None, None];
		}
		return undefined;
	})();
};
Option.prototype.flatten = function() {
	return (() => {
		const __match$17 = this;
		if (__match$17['~tag'] === 'Some') {
			const _ = __match$17.value;
			const value = __match$17.value.value;
			return Some(value);
		} else if (true) {
			return None;
		}
		return undefined;
	})();
};
Option.prototype.default = function() {
	return None;
};
