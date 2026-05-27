locals {
  cloud = values.cloud
}

unit "service" {
  source = "../../../../_modules/template"
  path   = "svc"

  values = {
    dep = format("../%s_base", local.cloud)
  }
}
