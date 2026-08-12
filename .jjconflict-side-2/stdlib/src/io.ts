import { nymphProtocolDisplay } from "std/box";

export const print = (x: unknown) => process.stdout.write(nymphProtocolDisplay(x).v);
export const println = (x: unknown) => console.log(nymphProtocolDisplay(x).v);
