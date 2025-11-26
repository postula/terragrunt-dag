locals {
  # file() nested inside multiple function calls
  config = merge(
    yamldecode(file("../base.yaml")),
    yamldecode(file("../override.yaml"))
  )
}
