locals {
  common = { env = "prod" }
  region = "eu"
}

unit "main" {
  source = "../../../_modules/template"
  path   = "main"

  values = merge(
    { a = "../../base" },
    local.common,
    { b = format("../../svc-%s", local.region) },
  )
}
