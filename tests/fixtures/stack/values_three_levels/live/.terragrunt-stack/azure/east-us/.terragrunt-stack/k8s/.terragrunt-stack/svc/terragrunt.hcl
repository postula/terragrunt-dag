terraform {
  source = "../../../../../../../_modules/template"
}

dependency "x" {
  config_path = values.dep
}
