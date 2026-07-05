export const compare_to_number = (first: number, second: number) => Math.sign(first - second)
export const compare_to_string = (first: string, second: string) => first.localeCompare(second)