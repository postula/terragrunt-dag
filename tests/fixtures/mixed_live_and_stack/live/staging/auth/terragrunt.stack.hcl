unit "azure__alpha" {
  source = "../../../_modules/auth"
  path   = "azure/alpha"

  values = {
    auth_type       = "azure"
    connection_name = "alpha"
  }
}

unit "saml__beta" {
  source = "../../../_modules/auth"
  path   = "saml/beta"

  values = {
    auth_type       = "saml"
    connection_name = "beta"
  }
}

unit "oauth_proxy" {
  source = "../../../_modules/oauth"
  path   = "oauth_proxy"

  values = {
    upstream_path = "../azure/alpha"
  }
}
