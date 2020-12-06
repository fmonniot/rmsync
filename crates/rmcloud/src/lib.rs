
#[derive(Debug)]
pub enum Error {}

pub struct Client;

pub fn make_client() -> Result<Client, Error> {
    Ok(Client)
}

impl Client {
    pub async fn upload(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
