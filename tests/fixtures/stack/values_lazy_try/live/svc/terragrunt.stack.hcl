locals {
  extras = try(values.optional_extras, ["fallback"])
}

unit "service" {
  source = "../../_modules/template"
  path   = "svc"

  values = {
    extras = local.extras
    dep    = "../base"
  }
}
