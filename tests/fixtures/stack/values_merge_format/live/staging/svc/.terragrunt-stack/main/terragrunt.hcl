terraform {
  source = "../../../../../_modules/template"
}

dependency "a" {
  config_path = values.a
}

dependency "b" {
  config_path = values.b
}
