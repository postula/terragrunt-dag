terraform {
  source = "../../../../_modules/template"
}

dependency "x" {
  config_path = values.all_locals.shared_dep
}
