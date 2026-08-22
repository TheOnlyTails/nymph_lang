import { NFloat, nymphFloatToInteger } from "std/box";

export const sin = (x: NFloat) => new NFloat(Math.sin(x.v));
export const cos = (x: NFloat) => new NFloat(Math.cos(x.v));
export const tan = (x: NFloat) => new NFloat(Math.tan(x.v));

export const asin = (x: NFloat) => new NFloat(Math.asin(x.v));
export const acos = (x: NFloat) => new NFloat(Math.acos(x.v));
export const atan = (x: NFloat) => new NFloat(Math.atan(x.v));

export const sinh = (x: NFloat) => new NFloat(Math.sinh(x.v));
export const cosh = (x: NFloat) => new NFloat(Math.cosh(x.v));
export const tanh = (x: NFloat) => new NFloat(Math.tanh(x.v));

export const asinh = (x: NFloat) => new NFloat(Math.asinh(x.v));
export const acosh = (x: NFloat) => new NFloat(Math.acosh(x.v));
export const atanh = (x: NFloat) => new NFloat(Math.atanh(x.v));

export const exp = (x: NFloat) => new NFloat(Math.exp(x.v));
export const ln = (x: NFloat) => new NFloat(Math.log(x.v));

export const atan2 = (y: NFloat, x: NFloat) => new NFloat(Math.atan2(y.v, x.v));
export const power_domain_error = (): never => {
	throw new RangeError("zero cannot be raised to a negative power");
};

export const floor = (x: NFloat) => nymphFloatToInteger(Math.floor(x.v), false);
export const ceil = (x: NFloat) => nymphFloatToInteger(Math.ceil(x.v), false);
export const round = (x: NFloat) => nymphFloatToInteger(Math.round(x.v), false);

export const max_float = Number.MAX_VALUE;
export const min_float = -Number.MAX_VALUE;
export const min_positive_float = Number.MIN_VALUE;
