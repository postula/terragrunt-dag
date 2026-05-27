terraform {
  source = "../../../../../_modules/auth"
}

# Dummy change to seed the example-gha-matrix workflow demo.
# Triggers azure_alpha as "changed" and consumer as cascade.
