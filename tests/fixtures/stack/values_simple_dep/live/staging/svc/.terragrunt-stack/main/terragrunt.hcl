terraform {
  source = "../../../../../_modules/template"
}

dependency "sibling" {
  config_path = values.dep
}
