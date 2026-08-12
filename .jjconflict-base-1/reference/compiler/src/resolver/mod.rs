// struct Resolver {
//   ast:
// }

// impl Resolver {
//     fn resolve_names(&mut self, )
//     pub fn resolve_ident(&mut self, ident: &Ident) -> Result<ResolvedIdent, ResolveError> {
//         match ident {
//             Ident::Unresolved(spanned_name) => {
//                 let name = &spanned_name.0;
//                 let span = spanned_name.1;

//                 // Perform resolution logic here
//                 // For example, look up the identifier in the current scope

//                 // If found, create a ResolvedIdent
//                 let resolved = ResolvedIdent {
//                     name: spanned_name.clone(),
//                     path: Vec::new(),
//                     id: self.next_id(),
//                 };
//                 Ok(resolved)
//             }
//         }
//     }
// }
