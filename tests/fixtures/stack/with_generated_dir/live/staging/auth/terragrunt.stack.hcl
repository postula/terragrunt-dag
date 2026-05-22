unit "azure__bdo" {
  source = "../../../_modules/auth"
  path   = "azure/bdo"

  values = {
    auth_type       = "azure"
    connection_name = "bdo"
  }
}

unit "saml__crelan" {
  source = "../../../_modules/auth"
  path   = "saml/crelan"

  values = {
    auth_type       = "saml"
    connection_name = "crelan"
  }
}
