import { display } from "./display";

export const print = (x: unknown) => process.stdout.write(display(x).v);
export const println = (x: unknown) => console.log(display(x).v);
