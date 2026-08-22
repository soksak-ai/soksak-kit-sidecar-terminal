use std::io;

use interprocess::local_socket::Name;

#[cfg(unix)]
pub fn local_name(address: &str) -> io::Result<Name<'_>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    address.to_fs_name::<GenericFilePath>()
}

#[cfg(windows)]
pub fn local_name(address: &str) -> io::Result<Name<'_>> {
    use interprocess::local_socket::ToFsName;
    use interprocess::os::windows::local_socket::NamedPipe;
    address.to_fs_name::<NamedPipe>()
}
