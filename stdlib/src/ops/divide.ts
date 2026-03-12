interface Divide<Other, Output> {
  func divide(other: Other): Output
}

impl Divide<Other = int, Output = float> for int {
  external func divide(other: int): float
}

impl Divide<Other = float, Output = float> for int {
  external func divide(other: float): float
}

impl Divide<Other = float, Output = float> for float {
  external func divide(other: float): float
}

impl Divide<Other = int, Output = float> for float {
  external func divide(other: int): float
}
